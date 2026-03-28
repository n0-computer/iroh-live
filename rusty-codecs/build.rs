fn main() {
    let has_opus = std::env::var("CARGO_FEATURE_OPUS").is_ok();
    let has_h264 = std::env::var("CARGO_FEATURE_H264").is_ok();
    let has_av1 = std::env::var("CARGO_FEATURE_AV1").is_ok();

    let has_pcm = std::env::var("CARGO_FEATURE_PCM").is_ok();

    let any_audio_codec = has_opus || has_pcm;
    let has_videotoolbox = std::env::var("CARGO_FEATURE_VIDEOTOOLBOX").is_ok();
    let has_vaapi = std::env::var("CARGO_FEATURE_VAAPI").is_ok();
    let has_v4l2 = std::env::var("CARGO_FEATURE_V4L2").is_ok();
    let has_mf = std::env::var("CARGO_FEATURE_MEDIA_FOUNDATION").is_ok();

    let any_video_codec =
        has_h264 || has_av1 || has_videotoolbox || has_vaapi || has_v4l2 || has_mf;

    if any_audio_codec {
        println!("cargo::rustc-cfg=any_audio_codec");
    }
    if any_video_codec {
        println!("cargo::rustc-cfg=any_video_codec");
    }
    if any_audio_codec && any_video_codec {
        println!("cargo::rustc-cfg=any_codec");
    }
}
