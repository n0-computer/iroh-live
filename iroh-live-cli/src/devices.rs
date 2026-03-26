use iroh_live::media::{
    AudioBackend,
    capture::{CameraCapturer, ScreenCapturer},
    codec::{AudioCodec, VideoCodec},
};

pub fn run() -> n0_error::Result {
    // Video codecs
    let codecs = VideoCodec::available();
    if codecs.is_empty() {
        println!("video codecs: (none compiled in)");
    } else {
        println!("video codecs:");
        for codec in &codecs {
            let hw = if codec.is_hardware() { " [hw]" } else { "" };
            println!("  {}{hw}", codec.display_name());
        }
    }

    // Audio codecs
    let audio_codecs = AudioCodec::available();
    if audio_codecs.is_empty() {
        println!("\naudio codecs: (none compiled in)");
    } else {
        println!("\naudio codecs:");
        for codec in &audio_codecs {
            println!("  {}", codec.display_name());
        }
    }

    // Cameras
    match CameraCapturer::list() {
        Ok(cameras) if !cameras.is_empty() => {
            println!("\ncameras:");
            for (i, cam) in cameras.iter().enumerate() {
                println!("  {i}: {}", cam.summary());
            }
        }
        Ok(_) => println!("\ncameras: (none found)"),
        Err(e) => println!("\ncameras: error listing — {e:#}"),
    }

    // Screens
    match ScreenCapturer::list() {
        Ok(screens) if !screens.is_empty() => {
            println!("\nscreens:");
            for (i, mon) in screens.iter().enumerate() {
                println!("  {i}: {}", mon.summary());
            }
        }
        Ok(_) => println!("\nscreens: (none found)"),
        Err(e) => println!("\nscreens: error listing — {e:#}"),
    }

    // Audio inputs
    let inputs = AudioBackend::list_inputs();
    if inputs.is_empty() {
        println!("\naudio inputs: (none found)");
    } else {
        println!("\naudio inputs:");
        for (i, dev) in inputs.iter().enumerate() {
            let dflt = if dev.is_default { " (default)" } else { "" };
            println!("  {i}: {}{dflt}", dev.name);
        }
    }

    // Audio outputs
    let outputs = AudioBackend::list_outputs();
    if outputs.is_empty() {
        println!("\naudio outputs: (none found)");
    } else {
        println!("\naudio outputs:");
        for (i, dev) in outputs.iter().enumerate() {
            let dflt = if dev.is_default { " (default)" } else { "" };
            println!("  {i}: {}{dflt}", dev.name);
        }
    }

    Ok(())
}
