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
use mp4_atom::{Atom, Codec as Entry, Encode, FourCC, Header, Moof, Moov, ReadAtom, ReadFrom};
use n0_error::{AnyError, StdResultExt, anyerr};
use tracing::warn;

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
    /// The picture size a video track declares.
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
        let Some(header) = Option::<Header>::read_from(reader).anyerr()? else {
            break;
        };
        match header.kind {
            Moov::KIND => moov = Some(Moov::read_atom(&header, reader).anyerr()?),
            Moof::KIND => fragments.push((start, Moof::read_atom(&header, reader).anyerr()?)),
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
        ids.insert(id, tracks.len());
        tracks.push(Track {
            video,
            width: visual.map_or(0, |visual| u32::from(visual.width)),
            height: visual.map_or(0, |visual| u32::from(visual.height)),
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

