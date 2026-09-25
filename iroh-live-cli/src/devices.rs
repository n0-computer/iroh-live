//! `irl devices`: lists capture and playback devices.
//!
//! Every printed identifier is a valid `--video` or `--audio` value for
//! `irl publish`.

#[cfg(feature = "playback")]
use iroh_live::media::AudioOutput;
use iroh_live::media::{audio, video};

/// Runs the `devices` command.
pub fn run(rt: &tokio::runtime::Runtime) -> n0_error::Result {
    rt.block_on(list());
    Ok(())
}

/// How many sizes to print per camera before summarising the rest.
///
/// A UVC camera often offers a dozen. Six show the range without a page of
/// output per device.
const MODES_SHOWN: usize = 6;

/// Prints the cameras, with the sizes and frame rates each one reports.
///
/// A rate the device lacks is silently replaced by the nearest one, so this
/// list is how a user picks `--renditions` values. Only Linux reports modes.
/// Other platforms report only the mode they picked, so their cameras print
/// the identifier alone.
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
        // A camera that reports no modes still has a usable identifier.
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

/// Formats one size and its frame rates as a line under its camera.
fn describe(mode: &video::capture::Mode) -> String {
    let size = format!("{}x{}", mode.width, mode.height);
    if mode.framerates.is_empty() {
        // The driver described a continuous range instead of listing rates.
        return format!("{size}  (rates not listed)");
    }
    let rates: Vec<String> = mode.framerates.iter().map(rate).collect();
    format!("{size}  {} fps", rates.join(", "))
}

/// Formats a frame rate for reading.
///
/// Drivers report exact ratios, and NTSC rates are fractions (24000/1001 is
/// 23.976 fps). Whole rates print as "30", others with two decimals.
/// `Rate`'s own `Display` would print "30/1".
fn rate(rate: &video::Rate) -> String {
    let fps = rate.as_f64();
    if (fps - fps.round()).abs() < 0.005 {
        format!("{}", fps.round() as u64)
    } else {
        format!("{fps:.2}")
    }
}

/// Prints every device section.
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

    // Window and app capture exist only on macOS (ScreenCaptureKit).
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
    section("audio outputs", AudioOutput::devices().await, |device| {
        // The id leads because it is what `irl watch --audio-output` takes.
        // Names alone do not tell a card's subdevices apart.
        let default = if device.default { " (default)" } else { "" };
        format!("{}  {}{default}", device.id, device.name)
    });
}

#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub mod rpicam {
    //! Raspberry Pi camera listing for `irl devices`.
    //!
    //! libcamera is reachable only through `rpicam-vid`, so this reports whether
    //! the binary is installed and which sensors it sees.

    use std::time::Duration;

    /// The binary that `iroh_live_media::VideoSource::rpicam` also runs.
    const RPICAM_VID: &str = "rpicam-vid";

    /// How long `--list-cameras` may run.
    ///
    /// Enumeration probes the I2C buses of the CSI connector and can hang on a
    /// half-seated ribbon cable. The other sections should still print.
    const LIST_TIMEOUT: Duration = Duration::from_secs(5);

    /// Returns the cameras `rpicam-vid --list-cameras` reports, as printable lines.
    ///
    /// The error is a note for the section, returned when `rpicam-vid` is
    /// missing, fails to run, or times out.
    pub(super) async fn cameras() -> Result<Vec<String>, String> {
        if !installed() {
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

    /// Turns one `--list-cameras` line into a printable entry.
    ///
    /// Camera lines look like `0 : imx219 [3280x2464 10-bit RGGB] (...)`. The
    /// index is dropped because `--video rpicam` takes no id.
    fn camera_line(line: &str) -> Option<String> {
        let (index, description) = line.split_once(" : ")?;
        index.trim().parse::<u32>().ok()?;
        Some(format!("rpicam  {}", description.trim()))
    }

    /// Returns whether `rpicam-vid` is on `PATH`.
    ///
    /// Checks for the binary only, because `--list-cameras` takes seconds.
    pub fn installed() -> bool {
        std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| dir.join(RPICAM_VID).is_file())
        })
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

/// Prints one section, with a failed enumeration as a note.
///
/// A machine with no camera driver still gets to see its microphones.
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
