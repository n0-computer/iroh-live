//! `irl devices`: list the capture and playback devices this machine offers.
//!
//! Every identifier printed here is one the `--video` and `--audio` specifiers
//! accept, so the output doubles as the argument reference for `irl publish`.

use iroh_live::media::{audio, video};

/// Runs the `devices` command.
pub fn run(rt: &tokio::runtime::Runtime) -> n0_error::Result {
    rt.block_on(list());
    Ok(())
}

/// Prints every section, in the order a publisher reaches for them.
async fn list() {
    section("cameras", video::capture::cameras().await, |camera| {
        format!("cam:{}  {}", camera.id, camera.name)
    });

    section("displays", video::capture::displays().await, |display| {
        format!(
            "screen:{}  {} ({}x{})",
            display.id, display.name, display.width, display.height
        )
    });

    // Windows and applications are ScreenCaptureKit concepts. Everywhere else
    // the enumeration returns `Unsupported`, so printing an empty section would
    // only be noise.
    #[cfg(target_os = "macos")]
    {
        section("windows", video::capture::windows().await, |window| {
            format!(
                "window:{}  {} - {} ({}x{})",
                window.id, window.app, window.title, window.width, window.height
            )
        });
        section("applications", video::capture::apps().await, |app| {
            format!("app:{}  {}", app.id, app.name)
        });
    }

    section("audio inputs", audio::capture::devices().await, |device| {
        let default = if device.default { " (default)" } else { "" };
        format!("mic:{}  {}{default}", device.id, device.name)
    });

    #[cfg(feature = "playback")]
    section(
        "audio outputs",
        audio::playback::devices().await,
        |device| {
            // The id is what `irl watch --audio-output` takes, so it leads: a
            // user copies the first token of a line rather than the prose after
            // it. The names alone do not distinguish a card's six subdevices.
            let default = if device.default { " (default)" } else { "" };
            format!("{}  {}{default}", device.id, device.name)
        },
    );
}

/// Prints one section, turning a failed enumeration into a note rather than
/// aborting the whole listing: a machine with no camera driver should still get
/// to see its microphones.
fn section<T, E: std::fmt::Display>(
    title: &str,
    devices: Result<Vec<T>, E>,
    line: impl Fn(&T) -> String,
) {
    println!("{title}:");
    match devices {
        Ok(devices) if devices.is_empty() => println!("  (none found)"),
        Ok(devices) => {
            for device in &devices {
                println!("  {}", line(device));
            }
        }
        Err(err) => println!("  (unavailable: {err})"),
    }
    println!();
}
