//! Minimal driver for the Waveshare 2.13" e-Paper V4 (SSD1680).
//!
//! The Touch e-Paper HAT ships V4 hardware. V4 uses the internal LUT, and its
//! refresh command 0x22 takes 0xF7 where V3 takes 0xC7. The `epd-waveshare`
//! crate supports only V2 and V3, so this follows Waveshare's Python driver
//! `epd2in13_V4.py`.
use std::{thread, time::Duration};

use embedded_hal::{
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};

/// Display width in pixels.
pub(crate) const WIDTH: u32 = 122;
/// Display height in pixels.
pub(crate) const HEIGHT: u32 = 250;

/// Frame buffer size in bytes: 16 bytes per row, 250 rows.
pub(crate) const BUF_LEN: usize = (WIDTH as usize).div_ceil(8) * HEIGHT as usize;

/// A Waveshare 2.13" e-Paper V4 display.
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
    pub(crate) fn new(spi: &mut SPI, dc: DC, rst: RST, busy: BUSY) -> Result<Self, SPI::Error> {
        let mut epd = Self {
            dc,
            rst,
            busy,
            _spi: std::marker::PhantomData,
        };
        epd.init_full(spi)?;
        Ok(epd)
    }

    fn send_command(&mut self, spi: &mut SPI, cmd: u8) -> Result<(), SPI::Error> {
        let _ = self.dc.set_low();
        spi.write(&[cmd])?;
        Ok(())
    }

    fn send_data(&mut self, spi: &mut SPI, data: &[u8]) -> Result<(), SPI::Error> {
        let _ = self.dc.set_high();
        // Chunks stay under the spidev transfer size limit.
        for chunk in data.chunks(4096) {
            spi.write(chunk)?;
        }
        Ok(())
    }

    fn send_command_data(&mut self, spi: &mut SPI, cmd: u8, data: &[u8]) -> Result<(), SPI::Error> {
        self.send_command(spi, cmd)?;
        self.send_data(spi, data)?;
        Ok(())
    }

    fn wait_busy(&mut self) {
        // The BUSY pin is high while the controller works.
        while let Ok(true) = self.busy.is_high() {
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn hw_reset(&mut self) {
        let _ = self.rst.set_high();
        thread::sleep(Duration::from_millis(20));
        let _ = self.rst.set_low();
        thread::sleep(Duration::from_millis(2));
        let _ = self.rst.set_high();
        thread::sleep(Duration::from_millis(20));
    }

    /// Runs the full-update init sequence from `epd2in13_V4.py`.
    fn init_full(&mut self, spi: &mut SPI) -> Result<(), SPI::Error> {
        self.hw_reset();
        self.wait_busy();

        self.send_command(spi, 0x12)?; // SW_RESET
        self.wait_busy();

        self.send_command_data(spi, 0x01, &[0xF9, 0x00, 0x00])?; // Driver output control
        self.send_command_data(spi, 0x11, &[0x03])?; // Data entry mode

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
        self.send_command_data(spi, 0x44, &[(x_start >> 3) as u8, (x_end >> 3) as u8])?;
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

    fn turn_on_display(&mut self, spi: &mut SPI) -> Result<(), SPI::Error> {
        self.send_command_data(spi, 0x22, &[0xF7])?; // V4 uses 0xF7, V3 uses 0xC7
        self.send_command(spi, 0x20)?; // Master activation
        self.wait_busy();
        Ok(())
    }

    /// Sends image data to the display RAM and triggers a full refresh.
    ///
    /// `buffer` holds [`BUF_LEN`] bytes, one bit per pixel: 1 is white, 0 is
    /// black. A full refresh takes about 2 seconds and flickers.
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

    /// Enters deep sleep. The image stays on screen at zero power.
    ///
    /// Create a new [`Epd2in13V4`] before the next display operation.
    pub(crate) fn sleep(&mut self, spi: &mut SPI) -> Result<(), SPI::Error> {
        self.send_command_data(spi, 0x10, &[0x01])?; // Deep sleep mode
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }
}
