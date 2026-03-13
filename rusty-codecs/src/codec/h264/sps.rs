//! SPS VUI patcher for low-latency H.264 decode.
//!
//! Patches SPS NALs in Annex B data to include VUI parameters with
//! `max_num_reorder_frames=0` and `max_dec_frame_buffering=1`. This tells
//! the decoder that no frame reordering is needed, eliminating the 3-5 frame
//! DPB buffering delay that causes periodic stutter at keyframe boundaries.
//!
//! Safe for Baseline/Constrained Baseline streams (no B-frames) where
//! reordering is never required. Setting these VUI fields for low-latency
//! decode is a well-known technique (also used by FFmpeg and GStreamer,
//! though they set the fields at the encoder side rather than patching
//! the bitstream on the decoder side as we do here).

/// Reads an unsigned exp-golomb coded value from a bitstream.
///
/// Returns `(value, bits_consumed)` or `None` if insufficient data.
fn read_ue(data: &[u8], bit_offset: usize) -> Option<(u32, usize)> {
    let total_bits = data.len() * 8;
    // Count leading zeros
    let mut zeros = 0;
    let mut pos = bit_offset;
    while pos < total_bits {
        if read_bit(data, pos) == 0 {
            zeros += 1;
            pos += 1;
        } else {
            break;
        }
    }
    if pos >= total_bits {
        return None;
    }
    // Skip the leading 1 bit
    pos += 1;
    if zeros == 0 {
        return Some((0, pos - bit_offset));
    }
    // Read `zeros` more bits
    if pos + zeros > total_bits {
        return None;
    }
    let mut val = 1u32;
    for _ in 0..zeros {
        val = (val << 1) | read_bit(data, pos) as u32;
        pos += 1;
    }
    Some((val - 1, pos - bit_offset))
}

/// Reads a single bit from a byte array at the given bit offset.
fn read_bit(data: &[u8], bit_offset: usize) -> u8 {
    let byte_idx = bit_offset / 8;
    let bit_idx = 7 - (bit_offset % 8);
    (data[byte_idx] >> bit_idx) & 1
}

/// Writes a single bit into a bit buffer.
fn write_bit(buf: &mut Vec<u8>, bit_pos: &mut usize, val: u8) {
    let byte_idx = *bit_pos / 8;
    let bit_idx = 7 - (*bit_pos % 8);
    if byte_idx >= buf.len() {
        buf.push(0);
    }
    if val != 0 {
        buf[byte_idx] |= 1 << bit_idx;
    }
    *bit_pos += 1;
}

/// Writes `n` bits from `val` (MSB first) into a bit buffer.
fn write_bits(buf: &mut Vec<u8>, bit_pos: &mut usize, val: u32, n: usize) {
    for i in (0..n).rev() {
        write_bit(buf, bit_pos, ((val >> i) & 1) as u8);
    }
}

/// Writes an unsigned exp-golomb coded value into a bit buffer.
fn write_ue(buf: &mut Vec<u8>, bit_pos: &mut usize, val: u32) {
    let code = val + 1;
    let bits = 32 - code.leading_zeros() as usize;
    // Write (bits-1) leading zeros
    for _ in 0..bits - 1 {
        write_bit(buf, bit_pos, 0);
    }
    // Write the code itself
    write_bits(buf, bit_pos, code, bits);
}

/// Copies `n` bits from `src` starting at `src_bit` to `dst` at `dst_bit`.
fn copy_bits(dst: &mut Vec<u8>, dst_bit: &mut usize, src: &[u8], src_bit: usize, n: usize) {
    for i in 0..n {
        write_bit(dst, dst_bit, read_bit(src, src_bit + i));
    }
}

/// Skips an unsigned exp-golomb coded value, returning bits consumed.
fn skip_ue(data: &[u8], bit_offset: usize) -> Option<usize> {
    read_ue(data, bit_offset).map(|(_, bits)| bits)
}

const HIGH_PROFILES: [u8; 6] = [100, 110, 122, 244, 44, 83];

/// Parses a raw SPS NAL unit (without start code) to find the bit offset of
/// `vui_parameters_present_flag`. Returns `(bit_offset, vui_present)`.
fn find_vui_flag_offset(sps: &[u8]) -> Option<(usize, bool)> {
    if sps.len() < 4 {
        return None;
    }

    // Byte 0: forbidden_zero_bit(1) + nal_ref_idc(2) + nal_unit_type(5)
    // Byte 1: profile_idc
    // Byte 2: constraint flags
    // Byte 3: level_idc
    let profile_idc = sps[1];
    let mut pos = 4 * 8; // start after fixed header

    // seq_parameter_set_id
    pos += skip_ue(sps, pos)?;

    // High profile extensions
    if HIGH_PROFILES.contains(&profile_idc)
        || profile_idc == 86
        || profile_idc == 118
        || profile_idc == 128
        || profile_idc == 138
        || profile_idc == 139
        || profile_idc == 134
    {
        let (chroma_format_idc, bits) = read_ue(sps, pos)?;
        pos += bits;
        if chroma_format_idc == 3 {
            pos += 1; // separate_colour_plane_flag
        }
        pos += skip_ue(sps, pos)?; // bit_depth_luma_minus8
        pos += skip_ue(sps, pos)?; // bit_depth_chroma_minus8
        pos += 1; // qpprime_y_zero_transform_bypass_flag

        // seq_scaling_matrix_present_flag
        let scaling_present = read_bit(sps, pos);
        pos += 1;
        if scaling_present != 0 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for i in 0..count {
                let list_present = read_bit(sps, pos);
                pos += 1;
                if list_present != 0 {
                    let size = if i < 6 { 16 } else { 64 };
                    // Skip scaling list (delta values as se)
                    pos += skip_scaling_list(sps, pos, size)?;
                }
            }
        }
    }

    // log2_max_frame_num_minus4
    pos += skip_ue(sps, pos)?;

    // pic_order_cnt_type
    let (poc_type, bits) = read_ue(sps, pos)?;
    pos += bits;

    match poc_type {
        0 => {
            pos += skip_ue(sps, pos)?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            pos += 1; // delta_pic_order_always_zero_flag
            pos += skip_se(sps, pos)?; // offset_for_non_ref_pic
            pos += skip_se(sps, pos)?; // offset_for_top_to_bottom_field
            let (num_ref_frames_in_poc_cycle, bits) = read_ue(sps, pos)?;
            pos += bits;
            for _ in 0..num_ref_frames_in_poc_cycle {
                pos += skip_se(sps, pos)?;
            }
        }
        _ => {} // poc_type == 2: nothing
    }

    // max_num_ref_frames
    pos += skip_ue(sps, pos)?;
    // gaps_in_frame_num_value_allowed_flag
    pos += 1;
    // pic_width_in_mbs_minus1
    pos += skip_ue(sps, pos)?;
    // pic_height_in_map_units_minus1
    pos += skip_ue(sps, pos)?;

    // frame_mbs_only_flag
    let frame_mbs_only = read_bit(sps, pos);
    pos += 1;
    if frame_mbs_only == 0 {
        pos += 1; // mb_adaptive_frame_field_flag
    }

    // direct_8x8_inference_flag
    pos += 1;

    // frame_cropping_flag
    let crop_flag = read_bit(sps, pos);
    pos += 1;
    if crop_flag != 0 {
        pos += skip_ue(sps, pos)?; // crop_left
        pos += skip_ue(sps, pos)?; // crop_right
        pos += skip_ue(sps, pos)?; // crop_top
        pos += skip_ue(sps, pos)?; // crop_bottom
    }

    // vui_parameters_present_flag
    if pos / 8 >= sps.len() {
        return None;
    }
    let vui_present = read_bit(sps, pos) != 0;
    Some((pos, vui_present))
}

/// Skips a signed exp-golomb value, returning bits consumed.
fn skip_se(data: &[u8], bit_offset: usize) -> Option<usize> {
    // se is encoded as ue with zigzag mapping; same bitstream format
    skip_ue(data, bit_offset)
}

/// Skips an H.264 scaling list of the given size, returning bits consumed.
fn skip_scaling_list(data: &[u8], bit_offset: usize, size: usize) -> Option<usize> {
    let mut pos = bit_offset;
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for _ in 0..size {
        if next_scale != 0 {
            let (delta, bits) = read_se(data, pos)?;
            pos += bits;
            next_scale = (last_scale + delta + 256) % 256;
        }
        last_scale = if next_scale == 0 {
            last_scale
        } else {
            next_scale
        };
    }
    Some(pos - bit_offset)
}

/// Reads a signed exp-golomb coded value.
fn read_se(data: &[u8], bit_offset: usize) -> Option<(i32, usize)> {
    let (ue_val, bits) = read_ue(data, bit_offset)?;
    let se_val = if ue_val % 2 == 0 {
        -(ue_val as i32 / 2)
    } else {
        (ue_val as i32 + 1) / 2
    };
    Some((se_val, bits))
}

/// Removes RBSP emulation prevention bytes (0x03 after 0x00 0x00).
fn rbsp_from_nal(nal: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut i = 0;
    while i < nal.len() {
        if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
            rbsp.push(0);
            rbsp.push(0);
            i += 3; // skip the 0x03
        } else {
            rbsp.push(nal[i]);
            i += 1;
        }
    }
    rbsp
}

/// Adds RBSP emulation prevention bytes where needed.
fn nal_from_rbsp(rbsp: &[u8]) -> Vec<u8> {
    let mut nal = Vec::with_capacity(rbsp.len() + rbsp.len() / 256);
    let mut zero_count = 0u32;
    for &byte in rbsp {
        if zero_count >= 2 && byte <= 3 {
            nal.push(0x03); // emulation prevention
            zero_count = 0;
        }
        nal.push(byte);
        if byte == 0 {
            zero_count += 1;
        } else {
            zero_count = 0;
        }
    }
    nal
}

/// Patches an SPS NAL unit (without start code) to include VUI with
/// `max_num_reorder_frames=0` and `max_dec_frame_buffering=1`.
///
/// Returns the patched NAL or `None` if parsing fails or VUI is already present
/// with bitstream restriction (assumed to be configured correctly).
fn patch_sps_nal_low_latency(sps_nal: &[u8]) -> Option<Vec<u8>> {
    // Work on RBSP (emulation prevention removed)
    let rbsp = rbsp_from_nal(sps_nal);

    let (vui_bit_offset, vui_present) = find_vui_flag_offset(&rbsp)?;

    if vui_present {
        // VUI already present — don't touch it (complex to navigate safely)
        return None;
    }

    // Build new SPS: copy everything up to and including vui_parameters_present_flag
    // (but set the flag to 1), then append minimal VUI, then RBSP trailing bits.
    let mut out = Vec::with_capacity(rbsp.len() + 4);
    let mut out_bit = 0usize;

    // Copy bits before vui_parameters_present_flag
    copy_bits(&mut out, &mut out_bit, &rbsp, 0, vui_bit_offset);

    // Set vui_parameters_present_flag = 1
    write_bit(&mut out, &mut out_bit, 1);

    // Write minimal VUI:
    // aspect_ratio_info_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // overscan_info_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // video_signal_type_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // chroma_loc_info_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // timing_info_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // nal_hrd_parameters_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // vcl_hrd_parameters_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // pic_struct_present_flag = 0
    write_bit(&mut out, &mut out_bit, 0);
    // bitstream_restriction_flag = 1
    write_bit(&mut out, &mut out_bit, 1);
    // motion_vectors_over_pic_boundaries_flag = 1
    write_bit(&mut out, &mut out_bit, 1);
    // max_bytes_per_pic_denom = 0 (ue)
    write_ue(&mut out, &mut out_bit, 0);
    // max_bits_per_mb_denom = 0 (ue)
    write_ue(&mut out, &mut out_bit, 0);
    // log2_max_mv_length_horizontal = 16 (ue) — spec default
    write_ue(&mut out, &mut out_bit, 16);
    // log2_max_mv_length_vertical = 16 (ue) — spec default
    write_ue(&mut out, &mut out_bit, 16);
    // max_num_reorder_frames = 0 (ue) — THE KEY FIX
    write_ue(&mut out, &mut out_bit, 0);
    // max_dec_frame_buffering = 1 (ue) — minimal DPB
    write_ue(&mut out, &mut out_bit, 1);

    // RBSP trailing bits: stop bit (1) + alignment zeros
    write_bit(&mut out, &mut out_bit, 1);
    while !out_bit.is_multiple_of(8) {
        write_bit(&mut out, &mut out_bit, 0);
    }

    // Convert RBSP back to NAL (add emulation prevention)
    Some(nal_from_rbsp(&out))
}

/// Patches all SPS NALs in Annex B data for low-latency decode.
///
/// For each SPS without VUI bitstream restriction, appends a minimal VUI
/// with `max_num_reorder_frames=0` to eliminate DPB buffering delay.
/// Returns `true` if any SPS was patched.
pub(crate) fn patch_sps_low_latency(annex_b: &mut Vec<u8>) -> bool {
    // Collect (start, end) byte ranges and patched NALs
    let mut patches: Vec<(usize, usize, Vec<u8>)> = Vec::new();

    let mut i = 0;
    while i < annex_b.len() {
        // Find start code
        let sc_len = if i + 3 <= annex_b.len() && annex_b[i] == 0 && annex_b[i + 1] == 0 {
            if annex_b[i + 2] == 1 {
                3
            } else if i + 4 <= annex_b.len() && annex_b[i + 2] == 0 && annex_b[i + 3] == 1 {
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
        // Find next start code
        let mut end = nal_start;
        while end < annex_b.len() {
            if end + 3 <= annex_b.len()
                && annex_b[end] == 0
                && annex_b[end + 1] == 0
                && (annex_b[end + 2] == 1
                    || (end + 4 <= annex_b.len() && annex_b[end + 2] == 0 && annex_b[end + 3] == 1))
            {
                break;
            }
            end += 1;
        }

        if end > nal_start {
            let nal = &annex_b[nal_start..end];
            let nal_type = nal[0] & 0x1F;
            if nal_type == 7
                && let Some(patched) = patch_sps_nal_low_latency(nal)
            {
                patches.push((nal_start, end, patched));
            }
        }
        i = end;
    }

    if patches.is_empty() {
        return false;
    }

    // Apply patches in reverse order to preserve earlier byte offsets
    for (start, end, patched) in patches.into_iter().rev() {
        annex_b.splice(start..end, patched);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb_read() {
        // ue(0) = 1 → single bit "1"
        assert_eq!(read_ue(&[0b1000_0000], 0), Some((0, 1)));
        // ue(1) = 010 → 3 bits
        assert_eq!(read_ue(&[0b0100_0000], 0), Some((1, 3)));
        // ue(2) = 011 → 3 bits
        assert_eq!(read_ue(&[0b0110_0000], 0), Some((2, 3)));
        // ue(3) = 00100 → 5 bits
        assert_eq!(read_ue(&[0b0010_0000], 0), Some((3, 5)));
    }

    #[test]
    fn exp_golomb_write_roundtrip() {
        for val in [0, 1, 2, 3, 4, 7, 15, 31, 100, 255] {
            let mut buf = Vec::new();
            let mut pos = 0;
            write_ue(&mut buf, &mut pos, val);
            let (decoded, bits) = read_ue(&buf, 0).unwrap();
            assert_eq!(decoded, val, "roundtrip failed for {val}");
            assert_eq!(bits, pos, "bit count mismatch for {val}");
        }
    }

    #[test]
    fn rbsp_emulation_prevention_roundtrip() {
        let original = vec![0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x03, 0x00, 0x00, 0x02];
        let rbsp = rbsp_from_nal(&original);
        let nal = nal_from_rbsp(&rbsp);
        let rbsp2 = rbsp_from_nal(&nal);
        assert_eq!(rbsp, rbsp2);
    }

    /// Constructs a valid Baseline SPS NAL (without start code) for testing.
    ///
    /// Baseline 640x360, level 3.0, poc_type=2, 1 ref frame, with cropping,
    /// vui_parameters_present_flag=0.
    fn make_test_sps() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut pos = 0;
        // NAL header: forbidden=0, nal_ref_idc=3, nal_unit_type=7
        write_bits(&mut buf, &mut pos, 0x67, 8);
        // profile_idc = 66
        write_bits(&mut buf, &mut pos, 66, 8);
        // constraint_set0_flag=1, constraint_set1_flag=1, rest=0
        write_bits(&mut buf, &mut pos, 0xC0, 8);
        // level_idc = 30
        write_bits(&mut buf, &mut pos, 30, 8);
        // seq_parameter_set_id = 0
        write_ue(&mut buf, &mut pos, 0);
        // log2_max_frame_num_minus4 = 0
        write_ue(&mut buf, &mut pos, 0);
        // pic_order_cnt_type = 2 (no sub-fields)
        write_ue(&mut buf, &mut pos, 2);
        // max_num_ref_frames = 1
        write_ue(&mut buf, &mut pos, 1);
        // gaps_in_frame_num_value_allowed_flag = 0
        write_bit(&mut buf, &mut pos, 0);
        // pic_width_in_mbs_minus1 = 39 (640/16 - 1)
        write_ue(&mut buf, &mut pos, 39);
        // pic_height_in_map_units_minus1 = 22 (ceil(360/16) - 1)
        write_ue(&mut buf, &mut pos, 22);
        // frame_mbs_only_flag = 1
        write_bit(&mut buf, &mut pos, 1);
        // direct_8x8_inference_flag = 0
        write_bit(&mut buf, &mut pos, 0);
        // frame_cropping_flag = 1 (368 → 360: crop 4 units bottom)
        write_bit(&mut buf, &mut pos, 1);
        write_ue(&mut buf, &mut pos, 0); // crop_left
        write_ue(&mut buf, &mut pos, 0); // crop_right
        write_ue(&mut buf, &mut pos, 0); // crop_top
        write_ue(&mut buf, &mut pos, 4); // crop_bottom
        // vui_parameters_present_flag = 0
        write_bit(&mut buf, &mut pos, 0);
        // RBSP stop bit + alignment
        write_bit(&mut buf, &mut pos, 1);
        while pos % 8 != 0 {
            write_bit(&mut buf, &mut pos, 0);
        }
        buf
    }

    #[test]
    fn find_vui_in_test_sps() {
        let sps = make_test_sps();
        let rbsp = rbsp_from_nal(&sps);
        let (offset, vui_present) = find_vui_flag_offset(&rbsp).unwrap();
        assert!(!vui_present, "test SPS should not have VUI");
        assert!(offset > 32, "VUI flag should be after fixed header");
    }

    #[test]
    fn patch_baseline_sps() {
        let sps = make_test_sps();
        let mut annex_b = vec![0x00, 0x00, 0x00, 0x01];
        annex_b.extend_from_slice(&sps);

        let original_len = annex_b.len();
        let patched = patch_sps_low_latency(&mut annex_b);
        assert!(patched, "should have patched the SPS");
        assert!(annex_b.len() > original_len, "patched SPS should be longer");

        // Verify the patched SPS can be parsed to find VUI
        let nals = super::super::annexb::parse_annex_b(&annex_b);
        assert_eq!(nals.len(), 1);
        let rbsp = rbsp_from_nal(nals[0]);
        let (offset, vui_present) = find_vui_flag_offset(&rbsp).unwrap();
        assert!(vui_present, "VUI should now be present");

        // Verify we can read the VUI bitstream_restriction fields
        let mut pos = offset + 1; // skip vui_parameters_present_flag
        // 8 flag bits (all 0) + bitstream_restriction_flag = 1
        for _ in 0..8 {
            assert_eq!(read_bit(&rbsp, pos), 0);
            pos += 1;
        }
        assert_eq!(read_bit(&rbsp, pos), 1, "bitstream_restriction_flag");
        pos += 1;
        // motion_vectors_over_pic_boundaries_flag
        assert_eq!(read_bit(&rbsp, pos), 1);
        pos += 1;
        // max_bytes_per_pic_denom = 0
        let (v, bits) = read_ue(&rbsp, pos).unwrap();
        assert_eq!(v, 0);
        pos += bits;
        // max_bits_per_mb_denom = 0
        let (v, bits) = read_ue(&rbsp, pos).unwrap();
        assert_eq!(v, 0);
        pos += bits;
        // log2_max_mv_length_horizontal = 16
        let (v, bits) = read_ue(&rbsp, pos).unwrap();
        assert_eq!(v, 16);
        pos += bits;
        // log2_max_mv_length_vertical = 16
        let (v, bits) = read_ue(&rbsp, pos).unwrap();
        assert_eq!(v, 16);
        pos += bits;
        // max_num_reorder_frames = 0
        let (v, bits) = read_ue(&rbsp, pos).unwrap();
        assert_eq!(v, 0, "max_num_reorder_frames should be 0");
        pos += bits;
        // max_dec_frame_buffering = 1
        let (v, _) = read_ue(&rbsp, pos).unwrap();
        assert_eq!(v, 1, "max_dec_frame_buffering should be 1");
    }

    #[test]
    fn no_double_patch() {
        let sps = make_test_sps();
        let mut annex_b = vec![0x00, 0x00, 0x00, 0x01];
        annex_b.extend_from_slice(&sps);

        assert!(patch_sps_low_latency(&mut annex_b));
        let after_first = annex_b.clone();
        // Second patch should be a no-op (VUI already present)
        assert!(!patch_sps_low_latency(&mut annex_b));
        assert_eq!(annex_b, after_first);
    }
}
