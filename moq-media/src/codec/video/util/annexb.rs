/// Parse an Annex B bitstream into individual NAL units (without start codes).
pub(crate) fn parse_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    while i < data.len() {
        // Find start code: 0x000001 or 0x00000001
        let sc_len = if i + 3 <= data.len() && data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                3
            } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                4
            } else {
                i += 1;
                continue;
            }
        } else {
            i += 1;
            continue;
        };

        let nal_start = i + sc_len;
        // Find next start code or end of data
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
        if end > nal_start {
            nals.push(&data[nal_start..end]);
        }
        i = end;
    }
    nals
}

/// Extract SPS (NAL type 7) and PPS (NAL type 8) from a slice of NAL units.
pub(crate) fn extract_sps_pps(nals: &[&[u8]]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut sps = None;
    let mut pps = None;
    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        match nal_type {
            7 => sps = Some(nal.to_vec()),
            8 => pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    Some((sps?, pps?))
}

/// Build an avcC (ISO 14496-15) decoder configuration record from SPS and PPS.
pub(crate) fn build_avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
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
pub(crate) fn avcc_to_annex_b(avcc: &[u8]) -> Option<Vec<u8>> {
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
pub(crate) fn length_prefixed_to_annex_b(data: &[u8]) -> Vec<u8> {
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

/// Convert Annex B start-code-separated NALs to length-prefixed (4-byte big-endian) format.
pub(crate) fn annex_b_to_length_prefixed(data: &[u8]) -> Vec<u8> {
    let nals = parse_annex_b(data);
    let total: usize = nals.iter().map(|n| 4 + n.len()).sum();
    let mut out = Vec::with_capacity(total);
    for nal in &nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}
