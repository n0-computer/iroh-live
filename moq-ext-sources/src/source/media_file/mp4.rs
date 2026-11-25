//! The sample index of an MP4 file, plain or fragmented.
//!
//! mp4-atom decodes the boxes. A plain MP4 lists every sample in the sample
//! tables of its `moov`, a fragmented one in the `moof` of each fragment.
//! Either way the index records where each sample lies and when it decodes
//! and presents, so a reader fetches one sample at a time and never holds
//! the file.

use std::{
    collections::HashMap,
    fmt,
    io::{Read, Seek, SeekFrom},
};

use bytes::Bytes;
use mp4_atom::{
    Atom, Codec as Entry, DecodeAtom, DecodeMaybe, Encode, FourCC, Header, Meta, Moof, Moov,
    ReadFrom, Traf, Trak, Udta,
};
use n0_error::{AnyError, StdResultExt, anyerr};
use tracing::{debug, warn};

/// The sample flag that marks a sample as not a sync sample (ISO/IEC 14496-12, 8.8.3.1).
const NON_SYNC: u32 = 0x0001_0000;

/// The MPEG-4 object type of AAC in an `esds` box.
const AAC_OBJECT_TYPE: u8 = 0x40;

/// One sample: where it lies, and when it decodes and presents, in microseconds.
///
/// Both times are after the track's edit list, so presentation starts at
/// zero, and a sample that decodes before it has a negative decode time.
/// While [`index`] reads the boxes, the times count the track's timescale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Sample {
    pub(super) offset: u64,
    pub(super) size: u32,
    pub(super) dts: i64,
    pub(super) pts: i64,
    pub(super) duration: u32,
    pub(super) keyframe: bool,
}

/// The codec of a track, with the record its decoder opens from.
#[derive(Debug, Clone)]
pub(super) enum Codec {
    /// H.264 with its avcC record.
    H264(Bytes),
    /// AAC with its AudioSpecificConfig.
    Aac(Bytes),
    /// A codec that is neither passed through nor decoded, by its name.
    Other(&'static str),
}

impl Codec {
    /// Returns the codec's name, for messages.
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::H264(_) => "H.264",
            Self::Aac(_) => "AAC",
            Self::Other(name) => name,
        }
    }
}

/// One audio or video track and its samples in decode order.
pub(super) struct Track {
    pub(super) video: bool,
    pub(super) codec: Codec,
    /// The picture size of a video track, from the first SPS for H.264 and
    /// otherwise from the sample entry.
    pub(super) width: u32,
    pub(super) height: u32,
    /// The sample rate and channel count of an audio track.
    pub(super) sample_rate: u32,
    pub(super) channels: u32,
    pub(super) samples: Vec<Sample>,
}

impl fmt::Debug for Track {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Track")
            .field("video", &self.video)
            .field("codec", &self.codec.name())
            .field("samples", &self.samples.len())
            .finish_non_exhaustive()
    }
}

/// Reads the sample index of the MP4 file `reader` holds.
///
/// Returns the audio and video tracks in file order. Skips the media data,
/// so this reads only the boxes that describe it. Fails for a sample that
/// lies past the end of the file, so no size taken from the file allocates
/// more than the file holds.
pub(super) fn index(reader: &mut (impl Read + Seek)) -> Result<Vec<Track>, AnyError> {
    let len = reader.seek(SeekFrom::End(0))?;
    reader.rewind()?;
    let mut moov = None;
    let mut fragments = Vec::new();
    loop {
        let start = reader.stream_position()?;
        // Some files end in a few bytes after the last box, too few for a
        // box header, such as a stray line break. A tail of 8 bytes or more
        // reads as a box header, and fails only if its size field is invalid
        // or it claims to be a `moov` or `moof`.
        let tail = len.saturating_sub(start);
        if tail < 8 {
            if tail > 0 {
                debug!(bytes = tail, "skipping bytes after the last box");
            }
            break;
        }
        let Some(header) = Option::<Header>::read_from(reader).anyerr()? else {
            break;
        };
        match header.kind {
            Moov::KIND => moov = Some(read_without_metadata::<Moov>(&header, reader)?),
            Moof::KIND => fragments.push((start, read_without_metadata::<Moof>(&header, reader)?)),
            // An `mdat` that runs to the end of the file ends the boxes.
            _ => match header.size {
                Some(size) => {
                    reader.seek(SeekFrom::Current(size as i64))?;
                }
                None => break,
            },
        }
    }
    let moov = moov.ok_or_else(|| anyerr!("no moov box, so this is not an MP4 file"))?;

    let mut tracks = Vec::new();
    let mut edits = Vec::new();
    let mut ids = HashMap::new();
    for trak in &moov.trak {
        let video = match trak.mdia.hdlr.handler {
            handler if handler == FourCC::new(b"vide") => true,
            handler if handler == FourCC::new(b"soun") => false,
            _ => continue,
        };
        let id = trak.tkhd.track_id;
        let stbl = &trak.mdia.minf.stbl;
        let entry = match stbl.stsd.codecs.as_slice() {
            [] => continue,
            [entry] => entry,
            entries => {
                return Err(anyerr!(
                    "track {id} has {} sample descriptions, as a file joined from several \
                     encodes has, and only one is supported",
                    entries.len()
                ));
            }
        };
        let codec = codec(entry)?;
        let visual = entry.visual();
        let declared = entry.audio().map_or((0, 0), |audio| {
            (
                u32::from(audio.sample_rate.integer()),
                u32::from(audio.channel_count),
            )
        });
        // The AudioSpecificConfig is exact where the sample entry may not be,
        // as for HE-AAC, whose entry declares the core rate.
        let (sample_rate, channels) = match &codec {
            Codec::Aac(config) => moq_mux::codec::aac::config(config).map_or(declared, |config| {
                (config.sample_rate, config.channel_count)
            }),
            _ => declared,
        };
        // The SPS is exact where the sample entry may not be: some files
        // declare 640x359 for pictures of 640x360. The encoder refuses the
        // odd height, while the decoder's pictures follow the SPS.
        let declared = visual.map_or((0, 0), |visual| {
            (u32::from(visual.width), u32::from(visual.height))
        });
        // moq-mux skips an SPS that does not parse and still returns the
        // config, without a size.
        let sps = match &codec {
            Codec::H264(record) => moq_mux::codec::h264::config(record)
                .ok()
                .and_then(|config| config.coded_width.zip(config.coded_height)),
            _ => None,
        };
        let (width, height) = sps.unwrap_or(declared);
        match sps {
            None if matches!(codec, Codec::H264(_)) => debug!(
                track = id,
                ?declared,
                "the avcC has no SPS that parses, using the sample entry's picture size"
            ),
            Some(sps) if sps != declared => debug!(
                track = id,
                ?declared,
                ?sps,
                "the sample entry's picture size differs from the SPS, using the SPS"
            ),
            _ => {}
        }
        ids.insert(id, tracks.len());
        tracks.push(Track {
            video,
            width,
            height,
            sample_rate,
            channels,
            codec,
            samples: sample_table(stbl, len)?,
        });
        edits.push((
            Edit::new(trak, moov.mvhd.timescale),
            trak.mdia.mdhd.timescale.max(1),
        ));
    }

    let trex: HashMap<u32, &mp4_atom::Trex> = moov
        .mvex
        .iter()
        .flat_map(|mvex| &mvex.trex)
        .map(|trex| (trex.track_id, trex))
        .collect();
    for (start, moof) in &fragments {
        // Without a base, a track fragment's data follows the previous one's.
        let mut data_end = *start;
        for traf in &moof.traf {
            let Some(&index) = ids.get(&traf.tfhd.track_id) else {
                continue;
            };
            let tfhd = &traf.tfhd;
            let base = match tfhd.base_data_offset {
                Some(base) => base,
                None if tfhd.default_base_is_moof => *start,
                None => data_end,
            };
            let trex = trex.get(&tfhd.track_id).copied();
            data_end = fragment(traf, base, trex, len, &mut tracks[index].samples)?;
        }
    }
    for (track, (edit, timescale)) in tracks.iter_mut().zip(&edits) {
        track.samples = edit.present(&track.samples, *timescale);
    }
    Ok(tracks)
}

/// The part of a track's media timeline that its edit list presents, in the track's timescale.
///
/// It covers the shape packagers write: empty edits that delay the track,
/// then one edit that starts the media at `media_time` at normal rate.
#[derive(Debug, Default)]
struct Edit {
    /// The media time that presents first.
    start: i64,
    /// The media time where the presentation ends, if the edit trims the end.
    end: Option<i64>,
    /// How long the empty edits hold the track back.
    delay: i64,
}

impl Edit {
    /// Reads the edit list of `trak`, in a movie of `movie_timescale` units per second.
    ///
    /// Several media edits splice the media, which one shift cannot express:
    /// the track then plays from the first edit's start to its end, with a
    /// warning. A rate other than 1 is ignored, also with a warning.
    fn new(trak: &mp4_atom::Trak, movie_timescale: u32) -> Self {
        let id = trak.tkhd.track_id;
        let to_media = |units: u64| {
            saturate(
                i128::from(units) * i128::from(trak.mdia.mdhd.timescale)
                    / i128::from(movie_timescale.max(1)),
            )
        };
        let mut entries = trak
            .edts
            .iter()
            .flat_map(|edts| &edts.elst)
            .flat_map(|elst| &elst.entries);
        let mut edit = Self::default();
        let (media_time, entry) = loop {
            let Some(entry) = entries.next() else {
                return edit;
            };
            match entry.media_time {
                Some(media_time) => break (media_time, entry),
                None => edit.delay = edit.delay.saturating_add(to_media(entry.segment_duration)),
            }
        };
        edit.start = saturate(i128::from(media_time));
        if entries.next().is_some() {
            warn!(
                track = id,
                "the edit list splices the media, so the track plays whole from the first edit"
            );
        } else if entry.segment_duration > 0 {
            edit.end = Some(edit.start.saturating_add(to_media(entry.segment_duration)));
        }
        if entry.media_rate.integer() != 1 || entry.media_rate.decimal() != 0 {
            warn!(
                track = id,
                "the edit list changes the rate, which is ignored"
            );
        }
        edit
    }

    /// Returns the samples this edit presents, with their times in microseconds.
    ///
    /// The presentation starts at the first keyframe at or after the edit's
    /// start, so a decoder needs no sample from before it. In decode order,
    /// it ends before the first sample that presents at or after the edit's
    /// end.
    fn present(&self, samples: &[Sample], timescale: u32) -> Vec<Sample> {
        let first = samples
            .iter()
            .position(|sample| sample.keyframe && sample.pts >= self.start)
            .unwrap_or(samples.len());
        let mut kept = &samples[first..];
        if let Some(end) = self.end {
            kept = &kept[..kept
                .iter()
                .position(|sample| sample.pts >= end)
                .unwrap_or(kept.len())];
        }
        let micros = |units: i64| {
            micros(
                units.saturating_sub(self.start).saturating_add(self.delay),
                timescale,
            )
        };
        kept.iter()
            .map(|sample| {
                let dts = micros(sample.dts);
                let end = micros(sample.dts.saturating_add(i64::from(sample.duration)));
                Sample {
                    dts,
                    pts: micros(sample.pts),
                    duration: u32::try_from(end - dts).unwrap_or(u32::MAX),
                    ..*sample
                }
            })
            .collect()
    }
}

/// Lists the samples of a plain MP4 track from its sample tables.
fn sample_table(stbl: &mp4_atom::Stbl, len: u64) -> Result<Vec<Sample>, AnyError> {
    let sizes = &stbl.stsz.samples;
    let count = match sizes {
        mp4_atom::StszSamples::Identical { count, size } => {
            if u64::from(*count) * u64::from((*size).max(1)) > len {
                return Err(anyerr!(
                    "the sample sizes add up to more than the file holds"
                ));
            }
            *count as usize
        }
        mp4_atom::StszSamples::Different { sizes } => sizes.len(),
    };
    if count == 0 {
        return Ok(Vec::new());
    }
    let size = |index: usize| match sizes {
        mp4_atom::StszSamples::Identical { size, .. } => *size,
        mp4_atom::StszSamples::Different { sizes } => sizes[index],
    };
    let chunks: Vec<u64> = match (&stbl.stco, &stbl.co64) {
        (Some(stco), _) => stco
            .entries
            .iter()
            .map(|&offset| u64::from(offset))
            .collect(),
        (None, Some(co64)) => co64.entries.clone(),
        (None, None) => return Err(anyerr!("a track has samples but no chunk offsets")),
    };

    let mut durations = stbl
        .stts
        .entries
        .iter()
        .flat_map(|entry| std::iter::repeat_n(entry.sample_delta, entry.sample_count as usize));
    let mut composition_offsets = stbl
        .ctts
        .iter()
        .flat_map(|ctts| &ctts.entries)
        .flat_map(|entry| std::iter::repeat_n(entry.sample_offset, entry.sample_count as usize));
    let sync = stbl.stss.as_ref().map(|stss| &stss.entries);
    let entries = &stbl.stsc.entries;
    let mut entry = 0;
    let mut samples = Vec::with_capacity(count);
    let mut dts = 0i64;
    'chunks: for (chunk, &chunk_offset) in chunks.iter().enumerate() {
        // Each sample-to-chunk entry runs until the next one's first chunk.
        let number = chunk as u32 + 1;
        while entries
            .get(entry + 1)
            .is_some_and(|next| next.first_chunk <= number)
        {
            entry += 1;
        }
        let per_chunk = entries
            .get(entry)
            .map_or(0, |entry| entry.samples_per_chunk);
        let mut offset = chunk_offset;
        for _ in 0..per_chunk {
            let index = samples.len();
            if index == count {
                break 'chunks;
            }
            let size = size(index);
            let duration = durations.next().unwrap_or(0);
            samples.push(Sample {
                offset,
                size,
                dts,
                pts: dts.saturating_add(composition_offsets.next().unwrap_or(0)),
                duration,
                // Without an stss box every sample is a sync sample.
                keyframe: sync.is_none_or(|sync| sync.binary_search(&(index as u32 + 1)).is_ok()),
            });
            offset = sample_end(offset, size, len)?;
            dts = dts.saturating_add(i64::from(duration));
        }
    }
    if samples.len() < count {
        return Err(anyerr!(
            "the chunk tables place {} of {count} samples",
            samples.len()
        ));
    }
    Ok(samples)
}

/// Appends the samples of one track fragment, and returns where its data ends.
///
/// `base` is where the fragment's data offsets count from. A fragment
/// without a `tfdt` continues from the last sample of `samples`.
fn fragment(
    traf: &mp4_atom::Traf,
    base: u64,
    trex: Option<&mp4_atom::Trex>,
    len: u64,
    samples: &mut Vec<Sample>,
) -> Result<u64, AnyError> {
    let tfhd = &traf.tfhd;
    let mut dts = match &traf.tfdt {
        Some(tfdt) => saturate(i128::from(tfdt.base_media_decode_time)),
        None => samples
            .last()
            .map_or(0, |last| last.dts.saturating_add(i64::from(last.duration))),
    };
    let mut offset = base;
    for trun in &traf.trun {
        if let Some(data_offset) = trun.data_offset {
            offset = base.saturating_add_signed(i64::from(data_offset));
        }
        for entry in &trun.samples {
            // A run of samples without data could otherwise list billions.
            if samples.len() as u64 >= len {
                return Err(anyerr!(
                    "a fragment lists more samples than the file has bytes"
                ));
            }
            let duration = entry
                .duration
                .or(tfhd.default_sample_duration)
                .unwrap_or_else(|| trex.map_or(0, |trex| trex.default_sample_duration));
            let size = entry
                .size
                .or(tfhd.default_sample_size)
                .unwrap_or_else(|| trex.map_or(0, |trex| trex.default_sample_size));
            let flags = entry
                .flags
                .or(tfhd.default_sample_flags)
                .unwrap_or_else(|| trex.map_or(0, |trex| trex.default_sample_flags));
            samples.push(Sample {
                offset,
                size,
                dts,
                pts: dts.saturating_add(entry.cts.unwrap_or(0)),
                duration,
                keyframe: flags & NON_SYNC == 0,
            });
            offset = sample_end(offset, size, len)?;
            dts = dts.saturating_add(i64::from(duration));
        }
    }
    Ok(offset)
}

/// Reads the `moov` or `moof` box that `header` opens, without metadata.
///
/// Decodes the body after [`without_metadata`] drops its metadata boxes.
fn read_without_metadata<T: Atom>(header: &Header, reader: &mut impl Read) -> Result<T, AnyError> {
    let mut body = Vec::new();
    match header.size {
        Some(size) => {
            // Reading through `take` grows the buffer only as far as the file goes.
            reader.take(size as u64).read_to_end(&mut body)?;
            if body.len() != size {
                return Err(anyerr!("the {} box runs past the end of the file", T::KIND));
            }
        }
        None => {
            reader.read_to_end(&mut body)?;
        }
    }
    let body = without_metadata(&body)?;
    let header = Header {
        kind: T::KIND,
        size: Some(body.len()),
    };
    T::decode_atom(&header, &mut body.as_slice()).anyerr()
}

/// Returns the child boxes in `body` without their `udta` and `meta` boxes.
///
/// Drops them inside each `trak` and `traf` too, which the index does not
/// read either. mp4-atom 0.17.1 refuses a repeated `ilst` item of a kind it
/// knows, a second `chpl` or `meta` in a `udta`, and a second `udta` or
/// `meta` in a `moov`, `trak` or `traf`. Players accept them, and some files
/// repeat them. mp4-atom 0.17.0 kept the last of a repeated `ilst` item;
/// 0.17.1 refuses it.
fn without_metadata(mut body: &[u8]) -> Result<Vec<u8>, AnyError> {
    let mut kept = Vec::with_capacity(body.len());
    while let Some(header) = Header::decode_maybe(&mut body).anyerr()? {
        let size = header.size.unwrap_or(body.len());
        let (child, rest) = body
            .split_at_checked(size)
            .ok_or_else(|| anyerr!("the {} box runs past the box that holds it", header.kind))?;
        body = rest;
        let filtered;
        let child = match header.kind {
            Udta::KIND | Meta::KIND => continue,
            Trak::KIND | Traf::KIND => {
                filtered = without_metadata(child)?;
                filtered.as_slice()
            }
            _ => child,
        };
        let header = Header {
            kind: header.kind,
            size: Some(child.len()),
        };
        header.encode(&mut kept).anyerr()?;
        kept.extend_from_slice(child);
    }
    // Keep what is too short for a box, such as the padding mp4-atom tolerates.
    kept.extend_from_slice(body);
    Ok(kept)
}

/// Returns where a sample of `size` bytes at `offset` ends, if it ends within the file's `len` bytes.
fn sample_end(offset: u64, size: u32, len: u64) -> Result<u64, AnyError> {
    offset
        .checked_add(u64::from(size))
        .filter(|&end| end <= len)
        .ok_or_else(|| {
            anyerr!("a sample at byte {offset} of {size} bytes lies past the end of the file")
        })
}

/// Returns the codec of a sample entry.
fn codec(entry: &Entry) -> Result<Codec, AnyError> {
    let avcc = match entry {
        Entry::Avc1(avc1) => &avc1.avcc,
        Entry::Avc3(avc3) => &avc3.avcc,
        Entry::Mp4a(mp4a) => {
            let config = &mp4a.esds.es_desc.dec_config;
            return Ok(match &config.dec_specific {
                Some(specific) if config.object_type_indication == AAC_OBJECT_TYPE => {
                    Codec::Aac(Bytes::copy_from_slice(&specific.raw))
                }
                _ => Codec::Other("another codec"),
            });
        }
        Entry::Hev1(_) | Entry::Hvc1(_) => return Ok(Codec::Other("H.265")),
        Entry::Vp09(_) => return Ok(Codec::Other("VP9")),
        Entry::Av01(_) => return Ok(Codec::Other("AV1")),
        Entry::Opus(_) => return Ok(Codec::Other("Opus")),
        _ => return Ok(Codec::Other("another codec")),
    };
    // The avcC box without its 8-byte header is the record a decoder takes.
    let mut record = Vec::new();
    avcc.encode(&mut record).anyerr()?;
    Ok(Codec::H264(Bytes::from(record).slice(8..)))
}

/// Returns `units` of `timescale` in microseconds.
fn micros(units: i64, timescale: u32) -> i64 {
    saturate(i128::from(units) * 1_000_000 / i128::from(timescale.max(1)))
}

/// Returns `value` limited to the range of an `i64`.
fn saturate(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[cfg(test)]
mod tests {
    //! Index tests on files built in memory, each a `moov` and `mdat` or `moof` boxes.

    use std::io::Cursor;

    use mp4_atom as atom;

    use super::*;

    /// The size of every test sample.
    const SIZE: u32 = 10;

    /// Returns a box of `kind` around `body`.
    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut data = (8 + body.len() as u32).to_be_bytes().to_vec();
        data.extend_from_slice(kind);
        data.extend_from_slice(body);
        data
    }

    /// Returns an `mdat` box with `samples` test samples.
    fn mdat(samples: u32) -> Vec<u8> {
        mp4_box(b"mdat", &vec![0; (samples * SIZE) as usize])
    }

    /// Returns sample tables of `count` samples of 100 ms in one chunk, which an `mdat` at the start holds.
    fn stbl(count: u32) -> atom::Stbl {
        atom::Stbl {
            stsd: atom::Stsd {
                // The index reads any codec, and this one is short to write.
                codecs: vec![
                    atom::Opus {
                        audio: atom::Audio {
                            data_reference_index: 1,
                            channel_count: 2,
                            sample_size: 16,
                            sample_rate: atom::FixedPoint::from(48_000),
                        },
                        dops: atom::Dops {
                            output_channel_count: 2,
                            pre_skip: 0,
                            input_sample_rate: 48_000,
                            output_gain: 0,
                            channel_mapping: None,
                        },
                        btrt: None,
                    }
                    .into(),
                ],
            },
            stts: atom::Stts {
                entries: vec![atom::SttsEntry {
                    sample_count: count,
                    sample_delta: 100,
                }],
            },
            stsc: atom::Stsc {
                entries: vec![atom::StscEntry {
                    first_chunk: 1,
                    samples_per_chunk: count,
                    sample_description_index: 1,
                }],
            },
            stsz: atom::Stsz {
                samples: atom::StszSamples::Identical { count, size: SIZE },
            },
            stco: Some(atom::Stco { entries: vec![8] }),
            ..Default::default()
        }
    }

    /// Returns a `moov` with one video track of timescale 1000 and the tables `stbl`.
    fn moov(stbl: atom::Stbl, edits: Vec<atom::ElstEntry>) -> atom::Moov {
        let trak = atom::Trak {
            tkhd: atom::Tkhd {
                track_id: 1,
                ..Default::default()
            },
            edts: (!edits.is_empty()).then_some(atom::Edts {
                elst: Some(atom::Elst { entries: edits }),
            }),
            mdia: atom::Mdia {
                mdhd: atom::Mdhd {
                    timescale: 1000,
                    language: "und".into(),
                    ..Default::default()
                },
                hdlr: atom::Hdlr {
                    handler: b"vide".into(),
                    name: String::new(),
                },
                minf: atom::Minf {
                    stbl,
                    ..Default::default()
                },
            },
            ..Default::default()
        };
        atom::Moov {
            trak: vec![trak],
            ..Default::default()
        }
    }

    /// Indexes an `mdat` of `samples` test samples followed by `moov`.
    fn index_plain(samples: u32, moov: &atom::Moov) -> Result<Vec<Sample>, AnyError> {
        let mut data = mdat(samples);
        moov.encode(&mut data).unwrap();
        let mut tracks = index(&mut Cursor::new(data))?;
        Ok(tracks.remove(0).samples)
    }

    /// Returns an edit list entry in the movie timescale of 1000.
    fn edit(segment_duration: u64, media_time: Option<u64>) -> atom::ElstEntry {
        atom::ElstEntry {
            segment_duration,
            media_time,
            media_rate: atom::FixedPoint::from(1),
        }
    }

    /// Returns the presentation times of `samples` in milliseconds.
    fn millis(samples: &[Sample]) -> Vec<i64> {
        samples.iter().map(|sample| sample.pts / 1000).collect()
    }

    /// An empty edit delays the track, and the media edit trims its start and end.
    #[test]
    fn edits_delay_and_trim() {
        let delayed = moov(stbl(10), vec![edit(500, None), edit(0, Some(200))]);
        let samples = index_plain(10, &delayed).unwrap();
        assert_eq!(
            millis(&samples),
            [500, 600, 700, 800, 900, 1000, 1100, 1200]
        );
        assert_eq!(samples[0].offset, 8 + 2 * u64::from(SIZE));

        let trimmed = moov(stbl(10), vec![edit(300, Some(200))]);
        assert_eq!(millis(&index_plain(10, &trimmed).unwrap()), [0, 100, 200]);

        // A splice plays the media whole from the first edit.
        let spliced = moov(stbl(10), vec![edit(300, Some(200)), edit(300, Some(700))]);
        assert_eq!(index_plain(10, &spliced).unwrap().len(), 8);
    }

    /// A version 1 `ctts` moves samples before their decode time.
    #[test]
    fn negative_composition_offsets() {
        let mut tables = stbl(4);
        tables.ctts = Some(atom::Ctts {
            entries: [0, 200, -100, -100]
                .map(|sample_offset| atom::CttsEntry {
                    sample_count: 1,
                    sample_offset,
                })
                .into(),
        });
        let samples = index_plain(4, &moov(tables, Vec::new())).unwrap();
        assert_eq!(millis(&samples), [0, 300, 100, 200]);
    }

    /// Chunk offsets in 64 bits place the samples as 32-bit ones do.
    #[test]
    fn co64_places_the_samples() {
        let mut tables = stbl(3);
        tables.stco = None;
        tables.co64 = Some(atom::Co64 { entries: vec![8] });
        let samples = index_plain(3, &moov(tables, Vec::new())).unwrap();
        let offsets: Vec<u64> = samples.iter().map(|sample| sample.offset).collect();
        assert_eq!(offsets, [8, 18, 28]);
    }

    /// Without an `stss` every sample is a keyframe, and with one only those it lists.
    #[test]
    fn sync_samples() {
        let all = index_plain(3, &moov(stbl(3), Vec::new())).unwrap();
        assert!(all.iter().all(|sample| sample.keyframe));
        let mut tables = stbl(3);
        tables.stss = Some(atom::Stss {
            entries: vec![1, 3],
        });
        let listed = index_plain(3, &moov(tables, Vec::new())).unwrap();
        let keyframes: Vec<bool> = listed.iter().map(|sample| sample.keyframe).collect();
        assert_eq!(keyframes, [true, false, true]);
    }

    /// Sizes and offsets past the end of the file fail the index instead of allocating.
    #[test]
    fn corrupt_sizes_fail() {
        let mut huge = stbl(3);
        huge.stsz.samples = atom::StszSamples::Identical {
            count: u32::MAX,
            size: SIZE,
        };
        assert!(index_plain(3, &moov(huge, Vec::new())).is_err());
        let mut past = stbl(3);
        past.stco = Some(atom::Stco {
            entries: vec![1 << 30],
        });
        assert!(index_plain(3, &moov(past, Vec::new())).is_err());
    }

    /// A track with two sample descriptions is refused.
    #[test]
    fn several_sample_descriptions_fail() {
        let mut tables = stbl(1);
        tables.stsd.codecs.push(tables.stsd.codecs[0].clone());
        assert!(index_plain(1, &moov(tables, Vec::new())).is_err());
    }

    /// A fragment without a `tfdt` continues the track where the last one ended.
    #[test]
    fn fragments_without_tfdt_continue() {
        let mut init = moov(stbl(0), Vec::new());
        init.mvex = Some(atom::Mvex {
            trex: vec![atom::Trex {
                track_id: 1,
                default_sample_description_index: 1,
                default_sample_duration: 100,
                default_sample_size: SIZE,
                default_sample_flags: 0,
            }],
            ..Default::default()
        });
        let mut data = Vec::new();
        init.encode(&mut data).unwrap();
        for (count, tfdt) in [(3, Some(0)), (2, None)] {
            let mut moof = atom::Moof {
                traf: vec![atom::Traf {
                    tfhd: atom::Tfhd {
                        track_id: 1,
                        default_base_is_moof: true,
                        ..Default::default()
                    },
                    tfdt: tfdt.map(|base_media_decode_time| atom::Tfdt {
                        base_media_decode_time,
                    }),
                    trun: vec![atom::Trun {
                        data_offset: Some(0),
                        samples: atom::TrunSamples::Inherited {
                            count,
                            first_flags: None,
                        },
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut encoded = Vec::new();
            moof.encode(&mut encoded).unwrap();
            // The samples follow the `moof` and the `mdat` header.
            moof.traf[0].trun[0].data_offset = Some(encoded.len() as i32 + 8);
            moof.encode(&mut data).unwrap();
            data.extend(mdat(count));
        }
        let samples = index(&mut Cursor::new(data)).unwrap().remove(0).samples;
        assert_eq!(millis(&samples), [0, 100, 200, 300, 400]);
    }

    /// Returns a `udta` whose `meta/ilst` repeats its copyright item, as some encoders write.
    fn duplicate_metadata() -> Vec<u8> {
        // A `data` box of text: type 1 for UTF-8, then the locale.
        let data = mp4_box(b"data", &[0, 0, 0, 1, 0, 0, 0, 0, b'x']);
        let item = mp4_box(b"cprt", &data);
        let ilst = mp4_box(b"ilst", &[item.clone(), item].concat());
        let hdlr = mp4_box(b"hdlr", &[&[0; 8][..], b"mdir", &[0; 13]].concat());
        // `meta` is a full box: version and flags precede its children.
        let meta = mp4_box(b"meta", &[&[0; 4][..], &hdlr, &ilst].concat());
        mp4_box(b"udta", &meta)
    }

    /// Repeated metadata items in the `moov` and a `trak` do not fail the index.
    #[test]
    fn duplicate_metadata_is_skipped() {
        let atom::Moov { mvhd, trak, .. } = moov(stbl(3), Vec::new());
        let mut mvhd_box = Vec::new();
        mvhd.encode(&mut mvhd_box).unwrap();
        let mut trak_body = Vec::new();
        trak[0].encode(&mut trak_body).unwrap();
        // Rebuild the `moov` and its `trak` with the metadata after their other children.
        let trak = mp4_box(b"trak", &[&trak_body[8..], &duplicate_metadata()].concat());
        let moov = mp4_box(b"moov", &[mvhd_box, trak, duplicate_metadata()].concat());
        // If this starts failing, mp4-atom accepts repeated metadata now,
        // and `read_without_metadata` can decode the box directly.
        assert!(matches!(
            <atom::Moov as atom::Decode>::decode(&mut moov.as_slice()),
            Err(atom::Error::DuplicateBox(_))
        ));

        let data = [mdat(3), moov].concat();
        let tracks = index(&mut Cursor::new(data)).unwrap();
        assert_eq!(millis(&tracks[0].samples), [0, 100, 200]);
    }

    /// Bytes after the last box, too few for a box header, do not fail the index.
    #[test]
    fn trailing_bytes_are_skipped() {
        let mut data = mdat(3);
        moov(stbl(3), Vec::new()).encode(&mut data).unwrap();
        data.extend_from_slice(b"\r\n");
        let tracks = index(&mut Cursor::new(data)).unwrap();
        assert_eq!(millis(&tracks[0].samples), [0, 100, 200]);
    }

    /// A tail long enough for a box header that claims a `moov` past the end of the file fails the index.
    #[test]
    fn a_long_junk_tail_fails() {
        let mut data = mdat(3);
        moov(stbl(3), Vec::new()).encode(&mut data).unwrap();
        data.extend_from_slice(&[0, 0, 1, 0, b'm', b'o', b'o', b'v', 0, 0]);
        assert!(index(&mut Cursor::new(data)).is_err());
    }

    /// Repeated metadata items in a `traf` do not fail the fragment.
    #[test]
    fn duplicate_metadata_in_a_fragment_is_skipped() {
        let mut mfhd = Vec::new();
        atom::Mfhd::default().encode(&mut mfhd).unwrap();
        let mut traf = Vec::new();
        atom::Traf::default().encode(&mut traf).unwrap();
        let traf = mp4_box(b"traf", &[&traf[8..], &duplicate_metadata()].concat());
        let moof = mp4_box(b"moof", &[mfhd, traf].concat());
        assert!(matches!(
            <atom::Moof as atom::Decode>::decode(&mut moof.as_slice()),
            Err(atom::Error::DuplicateBox(_))
        ));

        let mut reader = moof.as_slice();
        let header = <Header as atom::Decode>::decode(&mut reader).unwrap();
        let moof: Moof = read_without_metadata(&header, &mut reader).unwrap();
        assert_eq!(moof.traf.len(), 1);
    }

    /// Returns the position of the box `kind` in `data` at or after `from`.
    fn find(data: &[u8], from: usize, kind: &[u8; 4]) -> usize {
        from + data[from..]
            .windows(4)
            .position(|window| window == kind)
            .expect("the clip has the box")
    }

    /// Changes the height that the clip's `avc1` sample entry declares from 48 to 47.
    fn declare_height_47(data: &mut [u8]) {
        // The width and height follow the `avc1` kind after 24 bytes of
        // reserved fields, data reference index and pre-defined fields. The
        // `ftyp` names `avc1` as a brand too, so the search starts at `stsd`.
        let kind = find(data, find(data, 0, b"stsd"), b"avc1");
        let height = kind + 4 + 26;
        assert_eq!(data[height..height + 2], 48u16.to_be_bytes());
        data[height..height + 2].copy_from_slice(&47u16.to_be_bytes());
    }

    /// Returns the picture size of the first video track in `data`.
    fn video_size(data: Vec<u8>) -> (u32, u32) {
        let tracks = index(&mut Cursor::new(data)).unwrap();
        let video = tracks.iter().find(|track| track.video).unwrap();
        (video.width, video.height)
    }

    /// An H.264 track takes its picture size from the SPS, not from a sample entry that disagrees.
    #[test]
    fn the_sps_sets_the_picture_size() {
        let mut data = include_bytes!("../test_data/clip.mp4").to_vec();
        declare_height_47(&mut data);
        assert_eq!(video_size(data), (64, 48));
    }

    /// An SPS that does not parse leaves the picture size the sample entry declares.
    #[test]
    fn a_malformed_sps_keeps_the_declared_size() {
        let mut data = include_bytes!("../test_data/clip.mp4").to_vec();
        declare_height_47(&mut data);
        // After the `avcC` kind come 6 bytes of fields, the 2-byte length of
        // the first SPS, and its NAL header. Zeros then fail its first
        // Exp-Golomb code.
        let avcc = find(&data, 0, b"avcC");
        let len = usize::from(u16::from_be_bytes([data[avcc + 10], data[avcc + 11]]));
        data[avcc + 13..avcc + 12 + len].fill(0);
        let tracks = index(&mut Cursor::new(data)).unwrap();
        let video = tracks.iter().find(|track| track.video).unwrap();
        // moq-mux still reads the avcC, but finds no size in its SPS.
        let Codec::H264(record) = &video.codec else {
            panic!("the clip's video is H.264");
        };
        let config = moq_mux::codec::h264::config(record).unwrap();
        assert_eq!(config.coded_width.zip(config.coded_height), None);
        assert_eq!((video.width, video.height), (64, 47));
    }
}
