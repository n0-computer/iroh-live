fn main() {
    let has_opus = std::env::var("CARGO_FEATURE_OPUS").is_ok();
    let has_h264 = std::env::var("CARGO_FEATURE_H264").is_ok();
    let has_av1 = std::env::var("CARGO_FEATURE_AV1").is_ok();

    let any_audio_codec = has_opus;
    let has_videotoolbox = std::env::var("CARGO_FEATURE_VIDEOTOOLBOX").is_ok();
    let has_vaapi = std::env::var("CARGO_FEATURE_VAAPI").is_ok();
    let has_v4l2 = std::env::var("CARGO_FEATURE_V4L2").is_ok();
    let has_android = std::env::var("CARGO_FEATURE_ANDROID").is_ok();

    let any_video_codec =
        has_h264 || has_av1 || has_videotoolbox || has_vaapi || has_v4l2 || has_android;

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
