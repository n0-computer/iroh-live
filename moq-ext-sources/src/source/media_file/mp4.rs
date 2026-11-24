//! The sample index of an MP4 file, plain or fragmented.
//!
//! mp4-atom decodes the boxes. A plain MP4 lists every sample in the sample
//! tables of its `moov`, a fragmented one in the `moof` of each fragment.
//! Either way the index records where each sample lies and when it decodes
//! and presents, so a reader fetches one sample at a time and never holds
//! the file.

use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
};

use bytes::Bytes;
use mp4_atom::{Atom, Codec as Entry, Encode, FourCC, Header, Moof, Moov, ReadAtom, ReadFrom};
use n0_error::{AnyError, StdResultExt, anyerr};

/// The sample flag that marks a sample as not a sync sample (ISO/IEC 14496-12, 8.8.3.1).
const NON_SYNC: u32 = 0x0001_0000;

/// The MPEG-4 object type of AAC in an `esds` box.
const AAC_OBJECT_TYPE: u8 = 0x40;

/// The sample rates an AudioSpecificConfig indexes (ISO/IEC 14496-3, 1.6.3.4).
const AAC_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// One sample: where it lies, and when it decodes and presents in its track's timescale.
///
/// Both times are after the track's edit list, so presentation starts at
/// zero, and a sample that decodes before it has a negative decode time.
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
    H264 { avcc: Bytes },
    /// AAC with its AudioSpecificConfig.
    Aac { config: Bytes },
    /// A codec that is neither passed through nor decoded, by its name.
    Other(&'static str),
}

impl Codec {
    /// Returns the codec's name, for messages.
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::H264 { .. } => "H.264",
            Self::Aac { .. } => "AAC",
            Self::Other(name) => name,
        }
    }
}

/// One audio or video track and its samples in decode order.
#[derive(Debug)]
pub(super) struct Track {
    pub(super) video: bool,
    pub(super) timescale: u32,
    pub(super) codec: Codec,
    /// The picture size a video track declares.
    pub(super) width: u32,
    pub(super) height: u32,
    /// The sample rate and channel count an audio track declares.
    pub(super) sample_rate: u32,
    pub(super) channels: u32,
    pub(super) samples: Vec<Sample>,
}

/// Reads the sample index of the MP4 file `reader` holds.
///
/// Returns the audio and video tracks in file order. Skips the media data,
/// so this reads only the boxes that describe it.
pub(super) fn index(reader: &mut (impl Read + Seek)) -> Result<Vec<Track>, AnyError> {
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
    let mut ids = HashMap::new();
    for trak in &moov.trak {
        let video = match trak.mdia.hdlr.handler {
            handler if handler == FourCC::new(b"vide") => true,
            handler if handler == FourCC::new(b"soun") => false,
            _ => continue,
        };
        let stbl = &trak.mdia.minf.stbl;
        let Some(entry) = stbl.stsd.codecs.first() else {
            continue;
        };
        let edit = trak
            .edts
            .iter()
            .flat_map(|edts| &edts.elst)
            .flat_map(|elst| &elst.entries)
            .find_map(|entry| entry.media_time)
            .unwrap_or(0) as i64;
        let mut samples = sample_table(stbl)?;
        for sample in &mut samples {
            sample.dts -= edit;
            sample.pts -= edit;
        }
        ids.insert(trak.tkhd.track_id, (tracks.len(), edit));
        let visual = entry.visual();
        let audio = entry.audio();
        let mut track = Track {
            video,
            timescale: trak.mdia.mdhd.timescale.max(1),
            codec: codec(entry)?,
            width: visual.map_or(0, |visual| u32::from(visual.width)),
            height: visual.map_or(0, |visual| u32::from(visual.height)),
            sample_rate: audio.map_or(0, |audio| u32::from(audio.sample_rate.integer())),
            channels: audio.map_or(0, |audio| u32::from(audio.channel_count)),
            samples,
        };
        // The AudioSpecificConfig is exact where the sample entry may not be,
        // as for a rate above 65535 Hz.
        if let Entry::Mp4a(mp4a) = entry
            && let Some(specific) = &mp4a.esds.es_desc.dec_config.dec_specific
        {
            if let Some(&rate) = AAC_RATES.get(usize::from(specific.freq_index)) {
                track.sample_rate = rate;
            }
            match specific.chan_conf {
                0 => {}
                7 => track.channels = 8,
                channels => track.channels = u32::from(channels),
            }
        }
        tracks.push(track);
    }

    let trex: HashMap<u32, &mp4_atom::Trex> = moov
        .mvex
        .iter()
        .flat_map(|mvex| &mvex.trex)
        .map(|trex| (trex.track_id, trex))
        .collect();
    for (start, moof) in &fragments {
        for traf in &moof.traf {
            let Some(&(index, edit)) = ids.get(&traf.tfhd.track_id) else {
                continue;
            };
            let track = &mut tracks[index];
            // Where the track's decode timeline reached, before its edit.
            let next = track
                .samples
                .last()
                .map_or(0, |last| last.dts + edit + i64::from(last.duration));
            fragment(
                traf,
                *start,
                trex.get(&traf.tfhd.track_id).copied(),
                next,
                edit,
                &mut track.samples,
            );
        }
    }
    Ok(tracks)
}

/// Lists the samples of a plain MP4 track from its sample tables.
fn sample_table(stbl: &mp4_atom::Stbl) -> Result<Vec<Sample>, AnyError> {
    let sizes = match &stbl.stsz.samples {
        mp4_atom::StszSamples::Identical { count, size } => vec![*size; *count as usize],
        mp4_atom::StszSamples::Different { sizes } => sizes.clone(),
    };
    if sizes.is_empty() {
        return Ok(Vec::new());
    }
    let chunks: Vec<u64> = match (&stbl.stco, &stbl.co64) {
        (Some(stco), _) => stco
            .entries
            .iter()
            .map(|&offset| u64::from(offset))
            .collect(),
        (None, Some(co64)) => co64.entries.clone(),
        (None, None) => return Err(anyerr!("a track has samples but no chunk offsets")),
    };

    // Each sample-to-chunk entry runs until the next one's first chunk.
    let mut offsets = Vec::with_capacity(sizes.len());
    let entries = &stbl.stsc.entries;
    'chunks: for (chunk, &chunk_offset) in chunks.iter().enumerate() {
        let number = chunk as u32 + 1;
        let per_chunk = entries
            .iter()
            .rev()
            .find(|entry| entry.first_chunk <= number)
            .map_or(0, |entry| entry.samples_per_chunk);
        let mut offset = chunk_offset;
        for _ in 0..per_chunk {
            let Some(&size) = sizes.get(offsets.len()) else {
                break 'chunks;
            };
            offsets.push(offset);
            offset += u64::from(size);
        }
    }
    if offsets.len() < sizes.len() {
        return Err(anyerr!(
            "the chunk tables place {} of {} samples",
            offsets.len(),
            sizes.len()
        ));
    }

    let mut durations = stbl
        .stts
        .entries
        .iter()
        .flat_map(|entry| std::iter::repeat_n(entry.sample_delta, entry.sample_count as usize));
    let mut offsets_ct = stbl
        .ctts
        .iter()
        .flat_map(|ctts| &ctts.entries)
        .flat_map(|entry| std::iter::repeat_n(entry.sample_offset, entry.sample_count as usize));
    let sync = stbl.stss.as_ref().map(|stss| &stss.entries);
    let mut dts = 0i64;
    let samples = sizes
        .iter()
        .zip(offsets)
        .enumerate()
        .map(|(index, (&size, offset))| {
            let duration = durations.next().unwrap_or(0);
            let sample = Sample {
                offset,
                size,
                dts,
                pts: dts + offsets_ct.next().unwrap_or(0),
                duration,
                // Without an stss box every sample is a sync sample.
                keyframe: sync.is_none_or(|sync| sync.binary_search(&(index as u32 + 1)).is_ok()),
            };
            dts += i64::from(duration);
            sample
        })
        .collect();
    Ok(samples)
}

/// Appends the samples of one track fragment.
///
/// `start` is where the fragment's `moof` begins, which data offsets count
/// from unless the `tfhd` names a base. `next` is the decode time the track
/// reached, for a fragment without a `tfdt`.
fn fragment(
    traf: &mp4_atom::Traf,
    start: u64,
    trex: Option<&mp4_atom::Trex>,
    next: i64,
    edit: i64,
    samples: &mut Vec<Sample>,
) {
    let tfhd = &traf.tfhd;
    let base = tfhd.base_data_offset.unwrap_or(start);
    let mut dts = traf
        .tfdt
        .as_ref()
        .map_or(next, |tfdt| tfdt.base_media_decode_time as i64);
    let mut offset = base;
    for trun in &traf.trun {
        if let Some(data_offset) = trun.data_offset {
            offset = base.saturating_add_signed(i64::from(data_offset));
        }
        for entry in &trun.samples {
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
                dts: dts - edit,
                pts: dts + entry.cts.unwrap_or(0) - edit,
                duration,
                keyframe: flags & NON_SYNC == 0,
            });
            offset += u64::from(size);
            dts += i64::from(duration);
        }
    }
}

/// Returns the codec of a sample entry.
fn codec(entry: &Entry) -> Result<Codec, AnyError> {
    let avcc = match entry {
        Entry::Avc1(avc1) => &avc1.avcc,
        Entry::Avc3(avc3) => &avc3.avcc,
        Entry::Mp4a(mp4a) => {
            let config = &mp4a.esds.es_desc.dec_config;
            return Ok(match &config.dec_specific {
                Some(specific) if config.object_type_indication == AAC_OBJECT_TYPE => Codec::Aac {
                    config: Bytes::copy_from_slice(&specific.raw),
                },
                _ => Codec::Other("MPEG-4 audio other than AAC"),
            });
        }
        Entry::Hev1(_) | Entry::Hvc1(_) => return Ok(Codec::Other("H.265")),
        Entry::Vp08(_) => return Ok(Codec::Other("VP8")),
        Entry::Vp09(_) => return Ok(Codec::Other("VP9")),
        Entry::Av01(_) => return Ok(Codec::Other("AV1")),
        Entry::Mp3(_) => return Ok(Codec::Other("MP3")),
        Entry::Opus(_) => return Ok(Codec::Other("Opus")),
        Entry::Flac(_) => return Ok(Codec::Other("FLAC")),
        _ => return Ok(Codec::Other("an unsupported codec")),
    };
    // The avcC box without its 8-byte header is the record a decoder takes.
    let mut record = Vec::new();
    avcc.encode(&mut record).anyerr()?;
    Ok(Codec::H264 {
        avcc: Bytes::from(record).slice(8..),
    })
}
