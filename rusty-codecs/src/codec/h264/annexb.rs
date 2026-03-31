/// Iterates over NAL units in an Annex B bitstream, yielding each NAL
/// without its start code prefix.
pub(crate) struct AnnexBNalIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> AnnexBNalIter<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl<'a> Iterator for AnnexBNalIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let data = self.data;
        while self.pos < data.len() {
            // Find start code: 0x000001 or 0x00000001
            let i = self.pos;
            let sc_len = if i + 3 <= data.len() && data[i] == 0 && data[i + 1] == 0 {
                if data[i + 2] == 1 {
                    3
                } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                    4
                } else {
                    self.pos += 1;
                    continue;
                }
            } else {
                self.pos += 1;
                continue;
            };

            let nal_start = i + sc_len;
            let mut end = nal_start;
            while end < data.len() {
                if end + 3 <= data.len()
                    && data[end] == 0
                    && data[end + 1] == 0
                    && (data[end + 2] == 1
                        || (end + 4 <= data.len() && data[end + 2] == 0 && data[end + 3] == 1))
                {
                    break;
                }
                end += 1;
            }
            self.pos = end;
            if end > nal_start {
                return Some(&data[nal_start..end]);
            }
        }
        None
    }
}

/// Returns an iterator over NAL units in an Annex B bitstream.
pub(crate) fn annex_b_nals(data: &[u8]) -> AnnexBNalIter<'_> {
    AnnexBNalIter::new(data)
}

/// Parses an Annex B bitstream into individual NAL units (without start codes).
pub fn parse_annex_b(data: &[u8]) -> Vec<&[u8]> {
    annex_b_nals(data).collect()
}

/// Extract SPS (NAL type 7) and PPS (NAL type 8) from a slice of NAL units.
///
/// Returns borrowed slices into the original NAL data, avoiding per-keyframe
/// allocation.
pub fn extract_sps_pps<'a>(nals: &[&'a [u8]]) -> Option<(&'a [u8], &'a [u8])> {
    let mut sps = None;
    let mut pps = None;
    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        match nal_type {
            7 => sps = Some(*nal),
            8 => pps = Some(*nal),
            _ => {}
        }
    }
    Some((sps?, pps?))
}

/// Build an avcC (ISO 14496-15) decoder configuration record from SPS and PPS.
pub fn build_avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    // configurationVersion
    out.push(1);
    // AVCProfileIndication, profile_compatibility, AVCLevelIndication
    out.push(sps.get(1).copied().unwrap_or(66)); // Baseline profile
    out.push(sps.get(2).copied().unwrap_or(0));
    out.push(sps.get(3).copied().unwrap_or(30)); // Level 3.0
    // lengthSizeMinusOne = 3 (4-byte lengths) | reserved 0xFC
    out.push(0xFF);
    // numOfSequenceParameterSets = 1 | reserved 0xE0
    out.push(0xE1);
    // SPS length (big-endian u16)
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    // numOfPictureParameterSets = 1
    out.push(1);
    // PPS length (big-endian u16)
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    out
}

/// Extract SPS and PPS NAL units from an avcC (ISO 14496-15) configuration record
/// and return them as Annex B formatted data (with start codes).
pub fn avcc_to_annex_b(avcc: &[u8]) -> Option<Vec<u8>> {
    // Minimum avcC is 7 bytes header + at least 1 SPS entry
    if avcc.len() < 8 {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 5; // skip configurationVersion, profile, compat, level, lengthSizeMinusOne

    // SPS
    let num_sps = (avcc[i] & 0x1F) as usize;
    i += 1;
    for _ in 0..num_sps {
        if i + 2 > avcc.len() {
            return None;
        }
        let len = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
        i += 2;
        if i + len > avcc.len() {
            return None;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[i..i + len]);
        i += len;
    }

    // PPS
    if i >= avcc.len() {
        return None;
    }
    let num_pps = avcc[i] as usize;
    i += 1;
    for _ in 0..num_pps {
        if i + 2 > avcc.len() {
            return None;
        }
        let len = u16::from_be_bytes([avcc[i], avcc[i + 1]]) as usize;
        i += 2;
        if i + len > avcc.len() {
            return None;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[i..i + len]);
        i += len;
    }

    Some(out)
}

/// Convert length-prefixed (4-byte big-endian) NALs to Annex B format with 4-byte start codes.
pub fn length_prefixed_to_annex_b(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i + 4 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if i + len > data.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[i..i + len]);
        i += len;
    }
    out
}

/// Converts Annex B start-code-separated NALs to length-prefixed (4-byte big-endian) format.
///
/// Streams directly from the Annex B parser to the output buffer without
/// collecting an intermediate `Vec` of NAL slices.
pub(crate) fn annex_b_to_length_prefixed(data: &[u8]) -> Vec<u8> {
    // Worst case: every byte in the input is payload (no start codes shrink it).
    let mut out = Vec::with_capacity(data.len());
    for nal in annex_b_nals(data) {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_nal_4byte_start_code() {
        // 00 00 00 01 <NAL data>
        let data = [0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E];
        let nals = parse_annex_b(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], &[0x67, 0x42, 0x00, 0x1E]);
    }

    #[test]
    fn parse_single_nal_3byte_start_code() {
        let data = [0, 0, 1, 0x68, 0xCE, 0x38, 0x80];
        let nals = parse_annex_b(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], &[0x68, 0xCE, 0x38, 0x80]);
    }

    #[test]
    fn parse_multiple_nals() {
        // SPS + PPS with 4-byte start codes
        let mut data = vec![0, 0, 0, 1, 0x67, 0x42]; // SPS (type 7)
        data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]); // PPS (type 8)
        let nals = parse_annex_b(&data);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0], &[0x67, 0x42]);
        assert_eq!(nals[1], &[0x68, 0xCE]);
    }

    #[test]
    fn parse_empty() {
        let nals = parse_annex_b(&[]);
        assert!(nals.is_empty());
    }

    #[test]
    fn extract_sps_pps_found() {
        let sps = [0x67, 0x42, 0x00, 0x1E]; // NAL type 7
        let pps = [0x68, 0xCE, 0x38, 0x80]; // NAL type 8
        let idr = [0x65, 0x88, 0x84]; // NAL type 5 (IDR)
        let nals: Vec<&[u8]> = vec![&sps, &pps, &idr];
        let (found_sps, found_pps) = extract_sps_pps(&nals).unwrap();
        assert_eq!(found_sps, sps);
        assert_eq!(found_pps, pps);
    }

    #[test]
    fn extract_sps_pps_missing() {
        // Only IDR, no SPS/PPS
        let idr = [0x65, 0x88];
        let nals: Vec<&[u8]> = vec![&idr];
        assert!(extract_sps_pps(&nals).is_none());
    }

    #[test]
    fn extract_sps_pps_only_sps() {
        let sps = [0x67, 0x42];
        let nals: Vec<&[u8]> = vec![&sps];
        assert!(extract_sps_pps(&nals).is_none());
    }

    #[test]
    fn build_avcc_structure() {
        let sps = vec![0x67, 0x42, 0xC0, 0x1E];
        let pps = vec![0x68, 0xCE, 0x38, 0x80];
        let avcc = build_avcc(&sps, &pps);
        // configurationVersion
        assert_eq!(avcc[0], 1);
        // Profile from SPS[1]
        assert_eq!(avcc[1], 0x42);
        // Compatibility from SPS[2]
        assert_eq!(avcc[2], 0xC0);
        // Level from SPS[3]
        assert_eq!(avcc[3], 0x1E);
        // lengthSizeMinusOne | reserved
        assert_eq!(avcc[4], 0xFF);
        // numSPS | reserved
        assert_eq!(avcc[5], 0xE1);
        // SPS length (big-endian)
        assert_eq!(u16::from_be_bytes([avcc[6], avcc[7]]), 4);
        // SPS data
        assert_eq!(&avcc[8..12], &sps);
        // numPPS
        assert_eq!(avcc[12], 1);
        // PPS length
        assert_eq!(u16::from_be_bytes([avcc[13], avcc[14]]), 4);
        // PPS data
        assert_eq!(&avcc[15..19], &pps);
    }

    #[test]
    fn avcc_to_annex_b_roundtrip() {
        let sps = vec![0x67, 0x42, 0xC0, 0x1E];
        let pps = vec![0x68, 0xCE, 0x38, 0x80];
        let avcc = build_avcc(&sps, &pps);
        let annex_b = avcc_to_annex_b(&avcc).unwrap();
        let nals = parse_annex_b(&annex_b);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0], &sps[..]);
        assert_eq!(nals[1], &pps[..]);
    }

    #[test]
    fn avcc_to_annex_b_too_short() {
        assert!(avcc_to_annex_b(&[1, 2, 3]).is_none());
    }

    #[test]
    fn length_prefixed_to_annex_b_conversion() {
        // Two NALs: 3 bytes and 2 bytes
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_be_bytes());
        data.extend_from_slice(&[0x67, 0x42, 0x00]);
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&[0x68, 0xCE]);

        let annex_b = length_prefixed_to_annex_b(&data);
        let nals = parse_annex_b(&annex_b);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0], &[0x67, 0x42, 0x00]);
        assert_eq!(nals[1], &[0x68, 0xCE]);
    }

    #[test]
    fn annex_b_to_length_prefixed_roundtrip() {
        let mut original = vec![0, 0, 0, 1, 0x67, 0x42];
        original.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]);
        let length_prefixed = annex_b_to_length_prefixed(&original);
        let back = length_prefixed_to_annex_b(&length_prefixed);
        let nals = parse_annex_b(&back);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0], &[0x67, 0x42]);
        assert_eq!(nals[1], &[0x68, 0xCE]);
    }

    #[test]
    fn length_prefixed_empty() {
        let result = length_prefixed_to_annex_b(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn iterator_matches_collect() {
        // Verify the iterator produces the same results as parse_annex_b.
        let mut data = vec![0, 0, 0, 1, 0x67, 0x42, 0xC0];
        data.extend_from_slice(&[0, 0, 1, 0x68, 0xCE, 0x38]);
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);

        let collected: Vec<&[u8]> = annex_b_nals(&data).collect();
        let parsed = parse_annex_b(&data);
        assert_eq!(collected, parsed);
        assert_eq!(collected.len(), 3);
    }

    #[test]
    fn iterator_is_lazy() {
        // The iterator should yield NALs one at a time without collecting.
        let mut data = vec![0, 0, 0, 1, 0x67, 0x42];
        data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]);
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);

        let mut iter = annex_b_nals(&data);
        assert_eq!(iter.next().unwrap(), &[0x67, 0x42]);
        assert_eq!(iter.next().unwrap(), &[0x68, 0xCE]);
        assert_eq!(iter.next().unwrap(), &[0x65, 0x88]);
        assert!(iter.next().is_none());
    }
}
