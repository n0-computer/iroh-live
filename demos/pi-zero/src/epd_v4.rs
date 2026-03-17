/// Minimal driver for the Waveshare 2.13" e-Paper V4 (SSD1680-based).
///
/// The Touch e-Paper HAT ships with V4 hardware, which differs from V3 in:
/// - No external LUT needed (uses internal LUT)
/// - Display refresh command 0x22 takes 0xF7 (not 0xC7 like V3)
///
/// This driver implements the V4 protocol directly from Waveshare's reference
/// Python driver (`epd2in13_V4.py`), bypassing `epd-waveshare` which only
/// supports V2/V3.
use std::thread;
use std::time::Duration;

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;

/// Display resolution.
pub(crate) const WIDTH: u32 = 122;
pub(crate) const HEIGHT: u32 = 250;

/// Buffer size: ceil(122/8) * 250 = 16 * 250 = 4000 bytes.
pub(crate) const BUF_LEN: usize = (WIDTH as usize).div_ceil(8) * HEIGHT as usize;

pub(crate) struct Epd2in13V4<SPI, DC, RST, BUSY> {
    dc: DC,
    rst: RST,
    busy: BUSY,
    _spi: std::marker::PhantomData<SPI>,
}

impl<SPI, DC, RST, BUSY> Epd2in13V4<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    /// Creates a new driver and performs the full-update initialisation sequence.
    pub(crate) fn new(
        spi: &mut SPI,
        dc: DC,
        rst: RST,
        busy: BUSY,
    ) -> Result<Self, SPI::Error> {
        let mut epd = Self {
            dc,
            rst,
            busy,
            _spi: std::marker::PhantomData,
        };
        epd.init_full(spi)?;
        Ok(epd)
    }

    // --- low-level SPI helpers ---

    fn send_command(&mut self, spi: &mut SPI, cmd: u8) -> Result<(), SPI::Error> {
        let _ = self.dc.set_low();
        spi.write(&[cmd])?;
        Ok(())
    }

    fn send_data(&mut self, spi: &mut SPI, data: &[u8]) -> Result<(), SPI::Error> {
        let _ = self.dc.set_high();
        // Send in chunks to avoid SPI buffer limits.
        for chunk in data.chunks(4096) {
            spi.write(chunk)?;
        }
        Ok(())
    }

    fn send_command_data(
        &mut self,
        spi: &mut SPI,
        cmd: u8,
        data: &[u8],
    ) -> Result<(), SPI::Error> {
        self.send_command(spi, cmd)?;
        self.send_data(spi, data)?;
        Ok(())
    }

    // --- busy wait ---

    fn wait_busy(&mut self) {
        // BUSY pin: HIGH = busy, LOW = idle.
        while let Ok(true) = self.busy.is_high() {
            thread::sleep(Duration::from_millis(10));
        }
    }

    // --- hardware reset ---

    fn hw_reset(&mut self) {
        let _ = self.rst.set_high();
        thread::sleep(Duration::from_millis(20));
        let _ = self.rst.set_low();
        thread::sleep(Duration::from_millis(2));
        let _ = self.rst.set_high();
        thread::sleep(Duration::from_millis(20));
    }

    // --- init sequences (from Waveshare epd2in13_V4.py) ---

    fn init_full(&mut self, spi: &mut SPI) -> Result<(), SPI::Error> {
        self.hw_reset();
        self.wait_busy();

        self.send_command(spi, 0x12)?; // SW_RESET
        self.wait_busy();

        self.send_command_data(spi, 0x01, &[0xF9, 0x00, 0x00])?; // Driver output control
        self.send_command_data(spi, 0x11, &[0x03])?; // Data entry mode

        // Set window: full display.
        self.set_window(spi, 0, 0, WIDTH - 1, HEIGHT - 1)?;
        self.set_cursor(spi, 0, 0)?;

        self.send_command_data(spi, 0x3C, &[0x05])?; // Border waveform
        self.send_command_data(spi, 0x21, &[0x00, 0x80])?; // Display update control
        self.send_command_data(spi, 0x18, &[0x80])?; // Temperature sensor

        self.wait_busy();
        Ok(())
    }

    fn set_window(
        &mut self,
        spi: &mut SPI,
        x_start: u32,
        y_start: u32,
        x_end: u32,
        y_end: u32,
    ) -> Result<(), SPI::Error> {
        self.send_command_data(
            spi,
            0x44,
            &[(x_start >> 3) as u8, (x_end >> 3) as u8],
        )?;
        self.send_command_data(
            spi,
            0x45,
            &[
                y_start as u8,
                (y_start >> 8) as u8,
                y_end as u8,
                (y_end >> 8) as u8,
            ],
        )?;
        Ok(())
    }

    fn set_cursor(&mut self, spi: &mut SPI, x: u32, y: u32) -> Result<(), SPI::Error> {
        self.send_command_data(spi, 0x4E, &[x as u8])?;
        self.send_command_data(spi, 0x4F, &[y as u8, (y >> 8) as u8])?;
        Ok(())
    }

    // --- display operations ---

    fn turn_on_display(&mut self, spi: &mut SPI) -> Result<(), SPI::Error> {
        self.send_command_data(spi, 0x22, &[0xF7])?; // V4: 0xF7 (not 0xC7!)
        self.send_command(spi, 0x20)?; // Master activation
        self.wait_busy();
        Ok(())
    }

    /// Sends image data to the display RAM and triggers a full refresh.
    ///
    /// `buffer` must be exactly [`BUF_LEN`] bytes (4000). Each bit represents
    /// one pixel: 1 = white, 0 = black.
    ///
    /// The full refresh takes ~2 seconds and causes visible flickering.
    pub(crate) fn display(&mut self, spi: &mut SPI, buffer: &[u8]) -> Result<(), SPI::Error> {
        self.send_command(spi, 0x24)?; // WRITE_RAM
        self.send_data(spi, buffer)?;
        self.turn_on_display(spi)?;
        Ok(())
    }

    /// Clears the display to the given color (0xFF = white, 0x00 = black).
    pub(crate) fn clear(&mut self, spi: &mut SPI, color: u8) -> Result<(), SPI::Error> {
        let buf = [color; BUF_LEN];
        self.display(spi, &buf)?;
        Ok(())
    }

    /// Enters deep sleep mode. Image is retained at zero power draw.
    ///
    /// After calling this, the controller must be fully re-initialised before
    /// the next display operation (create a new [`Epd2in13V4`]).
    pub(crate) fn sleep(&mut self, spi: &mut SPI) -> Result<(), SPI::Error> {
        self.send_command_data(spi, 0x10, &[0x01])?; // Deep sleep mode
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }
}
