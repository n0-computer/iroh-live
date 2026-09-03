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
