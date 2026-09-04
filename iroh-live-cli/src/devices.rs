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

/// How many sizes a camera lists before the rest are summarised.
///
/// A UVC camera commonly offers a dozen, most of them steps nobody asks for
/// between the two anyone does. Enough to see the range and the shape of it
/// without a page of output per device.
const MODES_SHOWN: usize = 6;

/// Prints the cameras, with the sizes and frame rates each one reports.
///
/// The modes are what `--renditions 720p@60` is checked against by hand: a rate
/// the device does not have is not refused anywhere, it is quietly replaced by
/// the nearest one, so the only way to know what to ask for is to be told what
/// is there.
///
/// Only Linux answers. The other platforms' camera APIs report the mode they
/// picked rather than the modes they have, so those cameras print their
/// identifier alone and the rate is whatever opening the device gives.
async fn cameras() {
    let cameras = video::capture::cameras().await;
    println!("cameras:");
    let cameras = match cameras {
        Ok(cameras) if cameras.is_empty() => {
            println!("  (none found)\n");
            return;
        }
        Ok(cameras) => cameras,
        Err(err) => {
            println!("  (unavailable: {err})\n");
            return;
        }
    };

    for camera in &cameras {
        println!("  cam:{}  {}", camera.id, camera.name);
        // Enumerating opens nothing, so a camera another program is using still
        // answers. A camera that will not answer is not an error worth a line:
        // the identifier above it is still the thing a publisher needs.
        let Ok(modes) = video::capture::camera_modes(Some(&camera.id)).await else {
            continue;
        };
        for mode in modes.iter().take(MODES_SHOWN) {
            println!("      {}", describe(mode));
        }
        if modes.len() > MODES_SHOWN {
            println!("      ... and {} more", modes.len() - MODES_SHOWN);
        }
    }
    println!();
}

/// One size and the rates it offers, as a line under its camera.
fn describe(mode: &video::capture::Mode) -> String {
    let size = format!("{}x{}", mode.width, mode.height);
    if mode.framerates.is_empty() {
        // The driver described a continuous range rather than listing rates, so
        // the size is real and nothing here can say at which rates.
        return format!("{size}  (rates not listed)");
    }
    let rates: Vec<String> = mode.framerates.iter().map(u32::to_string).collect();
    format!("{size}  {} fps", rates.join(", "))
}

/// Prints every section, in the order a publisher reaches for them.
async fn list() {
    cameras().await;

    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    section(
        "raspberry pi cameras",
        rpicam::cameras().await,
        String::clone,
    );

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

#[cfg(all(target_os = "linux", feature = "rpicam"))]
mod rpicam {
    //! What `irl devices` can say about the Raspberry Pi camera.
    //!
    //! The libcamera stack is reachable only through `rpicam-vid`, so there is
    //! no device list to read: the two things worth reporting are whether the
    //! binary is installed and which sensors it can see.

    use std::{path::PathBuf, time::Duration};

    /// The subprocess we drive, the same one `moq_media::rpicam` starts.
    const RPICAM_VID: &str = "rpicam-vid";

    /// How long `--list-cameras` is given before we give up on it.
    ///
    /// Enumeration probes the I2C buses the CSI connector sits on, and a
    /// half-seated ribbon cable is enough to keep it there. `irl devices`
    /// should still print the microphones in that case.
    const LIST_TIMEOUT: Duration = Duration::from_secs(5);

    /// The cameras `rpicam-vid` reports, described as `irl devices` prints
    /// them.
    ///
    /// `--list-cameras` enumerates and exits without starting a capture, which
    /// is what keeps this cheap enough to run on every `irl devices`.
    ///
    /// # Errors
    ///
    /// Returns a note for the section when `rpicam-vid` is not installed, will
    /// not run, or does not finish enumerating.
    pub(super) async fn cameras() -> Result<Vec<String>, String> {
        if on_path(RPICAM_VID).is_none() {
            return Err(format!("{RPICAM_VID} is not on PATH"));
        }
        let listing = tokio::process::Command::new(RPICAM_VID)
            .arg("--list-cameras")
            .output();
        let listing = match tokio::time::timeout(LIST_TIMEOUT, listing).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => return Err(format!("{RPICAM_VID} would not run: {err}")),
            Err(_) => {
                return Err(format!(
                    "{RPICAM_VID} --list-cameras did not finish within {}s",
                    LIST_TIMEOUT.as_secs()
                ));
            }
        };
        Ok(String::from_utf8_lossy(&listing.stdout)
            .lines()
            .filter_map(camera_line)
            .collect())
    }

    /// Turns one line of the `--list-cameras` listing into a printable entry.
    ///
    /// The listing indents the mode table under a header per camera, so the
    /// entries are the lines shaped `0 : imx219 [3280x2464 10-bit RGGB] (...)`.
    /// The index is dropped: `--video rpicam` takes no id, because `rpicam-vid`
    /// picks the camera itself.
    fn camera_line(line: &str) -> Option<String> {
        let (index, description) = line.split_once(" : ")?;
        index.trim().parse::<u32>().ok()?;
        Some(format!("rpicam  {}", description.trim()))
    }

    /// The first entry of `PATH` holding a file named `name`.
    fn on_path(name: &str) -> Option<PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn camera_lines_are_the_numbered_ones() {
            assert_eq!(
                camera_line("0 : imx219 [3280x2464 10-bit RGGB] (/base/soc/i2c0mux)"),
                Some("rpicam  imx219 [3280x2464 10-bit RGGB] (/base/soc/i2c0mux)".to_string())
            );
            assert_eq!(camera_line("Available cameras"), None);
            assert_eq!(camera_line("    Modes: 'SRGGB10_CSI2P' : 640x480"), None);
        }
    }
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
