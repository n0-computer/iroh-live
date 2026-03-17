/// Waveshare 2.13" Touch e-Paper HAT driver (V4 hardware).
///
/// Uses a custom V4 driver ([`crate::epd_v4`]) instead of `epd-waveshare`,
/// which only supports V2/V3. The Touch HAT ships with V4 hardware that needs
/// a different display refresh command (0xF7 vs 0xC7) and no external LUT.
///
/// # E-paper precautions (from Waveshare datasheet)
///
/// - **Full refresh only.** We never use partial refresh.
/// - **Sleep after refresh.** Display sleeps immediately after every update.
/// - **Minimum 180 s between full refreshes.** Periodic task runs every 12 h.
/// - **Refresh at least once every 24 h.** Periodic task satisfies this.
/// - **Clear before long-term storage.** [`clear_display`] clears and sleeps.
/// - **Re-init after sleep.** Every function creates a fresh driver instance.
///
/// Pin mapping (Waveshare 2.13" Touch HAT):
///
/// | Function | BCM GPIO | Physical pin |
/// |----------|----------|--------------|
/// | SPI MOSI | 10       | 19           |
/// | SPI SCLK | 11       | 23           |
/// | SPI CE0  | 8        | 24           |
/// | DC       | 25       | 22           |
/// | RST      | 17       | 11           |
/// | BUSY     | 24       | 18           |
use embedded_graphics::{
    geometry::{Point, Size},
    mono_font::{MonoTextStyle, ascii::FONT_4X6},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::Text,
};
use gpio_cdev::{Chip, LineRequestFlags};
use linux_embedded_hal::{CdevPin, SpidevDevice};
use qrcode::QrCode;

use crate::epd_v4::{self, Epd2in13V4};

/// GPIO chip device (default on Raspberry Pi).
const GPIO_CHIP: &str = "/dev/gpiochip0";

/// SPI device for the e-paper display.
const SPI_DEV: &str = "/dev/spidev0.0";

/// BCM GPIO pin numbers for the e-paper HAT.
const PIN_DC: u32 = 25;
const PIN_RST: u32 = 17;
const PIN_BUSY: u32 = 24;

type Epd = Epd2in13V4<SpidevDevice, CdevPin, CdevPin, CdevPin>;

/// A 1-bit framebuffer matching the display dimensions.
///
/// Uses `embedded-graphics` with `BinaryColor` and converts to the EPD's
/// wire format (1 = white, 0 = black) on flush.
struct DisplayBuffer {
    /// Pixel buffer: BinaryColor::Off = white (bit 1), BinaryColor::On = black (bit 0).
    buf: [u8; epd_v4::BUF_LEN],
}

impl DisplayBuffer {
    fn new_white() -> Self {
        Self {
            buf: [0xFF; epd_v4::BUF_LEN],
        }
    }

    fn buffer(&self) -> &[u8] {
        &self.buf
    }
}

impl DrawTarget for DisplayBuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        let width = epd_v4::WIDTH;
        let height = epd_v4::HEIGHT;
        let line_bytes = width.div_ceil(8) as usize;

        for embedded_graphics::Pixel(point, color) in pixels {
            let x = point.x;
            let y = point.y;
            if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                continue;
            }
            let x = x as usize;
            let y = y as usize;
            let byte_idx = y * line_bytes + x / 8;
            let bit_mask = 0x80 >> (x % 8);

            match color {
                BinaryColor::Off => self.buf[byte_idx] |= bit_mask, // white = 1
                BinaryColor::On => self.buf[byte_idx] &= !bit_mask, // black = 0
            }
        }
        Ok(())
    }
}

impl OriginDimensions for DisplayBuffer {
    fn size(&self) -> Size {
        Size::new(epd_v4::WIDTH, epd_v4::HEIGHT)
    }
}

/// Opens the SPI device and GPIO lines, returning a ready-to-use EPD handle.
fn open_epd() -> anyhow::Result<(SpidevDevice, Epd)> {
    tracing::debug!(spi = SPI_DEV, "opening SPI device");
    let mut spi = SpidevDevice::open(SPI_DEV)?;
    use linux_embedded_hal::spidev::{SpiModeFlags, SpidevOptions};
    let opts = SpidevOptions::new()
        .bits_per_word(8)
        .max_speed_hz(4_000_000)
        .mode(SpiModeFlags::SPI_MODE_0)
        .build();
    spi.configure(&opts)?;
    tracing::debug!("SPI configured: 4 MHz, mode 0");

    tracing::debug!(chip = GPIO_CHIP, "opening GPIO chip");
    let mut chip = Chip::new(GPIO_CHIP)?;

    tracing::debug!(pin = PIN_DC, "requesting DC pin (output)");
    let dc_line = chip
        .get_line(PIN_DC)?
        .request(LineRequestFlags::OUTPUT, 0, "epaper-dc")?;
    let dc = CdevPin::new(dc_line)?;

    tracing::debug!(pin = PIN_RST, "requesting RST pin (output)");
    let rst_line = chip
        .get_line(PIN_RST)?
        .request(LineRequestFlags::OUTPUT, 1, "epaper-rst")?;
    let rst = CdevPin::new(rst_line)?;

    tracing::debug!(pin = PIN_BUSY, "requesting BUSY pin (input)");
    let busy_line = chip
        .get_line(PIN_BUSY)?
        .request(LineRequestFlags::INPUT, 0, "epaper-busy")?;
    let busy = CdevPin::new(busy_line)?;

    tracing::debug!("initialising EPD controller (V4 protocol)");
    let epd = Epd2in13V4::new(&mut spi, dc, rst, busy)
        .map_err(|e| anyhow::anyhow!("EPD init failed: {e}"))?;
    tracing::debug!("EPD controller ready");

    Ok((spi, epd))
}

/// Generates a QR code from `data` and displays it on the e-paper HAT.
pub(crate) fn display_qr(data: &str) -> anyhow::Result<()> {
    tracing::info!("display_qr: starting");
    let (mut spi, mut epd) = open_epd()?;

    let mut display = DisplayBuffer::new_white();

    // --- QR code ---
    let code = QrCode::new(data.as_bytes())?;
    let modules = code.width();
    tracing::debug!(modules, data_len = data.len(), "QR code generated");

    // Scale to fit within the narrower display dimension (122px) with margin.
    let max_qr_px = (epd_v4::WIDTH - 10) as usize;
    let scale = std::cmp::max(1, max_qr_px / modules);
    let qr_px = modules * scale;

    let x_offset = ((epd_v4::WIDTH as usize).saturating_sub(qr_px)) / 2;
    let y_offset = ((epd_v4::HEIGHT as usize - 14).saturating_sub(qr_px)) / 2;
    tracing::debug!(scale, qr_px, x_offset, y_offset, "QR layout computed");

    let colors = code.to_colors();
    let dark_count = colors.iter().filter(|&&c| c == qrcode::Color::Dark).count();
    tracing::debug!(
        total_modules = colors.len(),
        dark_modules = dark_count,
        "drawing QR modules"
    );

    for (idx, &dark) in colors.iter().enumerate() {
        let mx = idx % modules;
        let my = idx / modules;
        if dark == qrcode::Color::Dark {
            Rectangle::new(
                Point::new(
                    (x_offset + mx * scale) as i32,
                    (y_offset + my * scale) as i32,
                ),
                Size::new(scale as u32, scale as u32),
            )
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(&mut display)?;
        }
    }

    // Small label below the QR code.
    let label_y = (y_offset + qr_px + 8) as i32;
    let style = MonoTextStyle::new(&FONT_4X6, BinaryColor::On);
    Text::new("iroh-live", Point::new(x_offset as i32, label_y), style).draw(&mut display)?;

    let buf = display.buffer();
    let non_ff = buf.iter().filter(|&&b| b != 0xFF).count();
    tracing::debug!(
        buffer_len = buf.len(),
        non_white_bytes = non_ff,
        "display buffer ready"
    );

    tracing::info!("sending frame to EPD (V4 full refresh, ~2 s)");
    epd.display(&mut spi, display.buffer())
        .map_err(|e| anyhow::anyhow!("display failed: {e}"))?;
    tracing::info!("EPD refresh complete");

    tracing::debug!("putting EPD to sleep");
    epd.sleep(&mut spi)
        .map_err(|e| anyhow::anyhow!("sleep failed: {e}"))?;

    Ok(())
}

/// Fills the entire display with a checkerboard pattern for diagnostics.
pub(crate) fn display_test_pattern() -> anyhow::Result<()> {
    tracing::info!("display_test_pattern: starting");
    let (mut spi, mut epd) = open_epd()?;

    let mut display = DisplayBuffer::new_white();

    // Fill with black first.
    Rectangle::new(Point::zero(), Size::new(epd_v4::WIDTH, epd_v4::HEIGHT))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(&mut display)?;

    // Draw white squares for checkerboard.
    let cell = 20u32;
    for y in (0..epd_v4::HEIGHT).step_by(cell as usize * 2) {
        for x in (0..epd_v4::WIDTH).step_by(cell as usize * 2) {
            Rectangle::new(Point::new(x as i32, y as i32), Size::new(cell, cell))
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(&mut display)?;
        }
    }
    for y in (cell..epd_v4::HEIGHT).step_by(cell as usize * 2) {
        for x in (cell..epd_v4::WIDTH).step_by(cell as usize * 2) {
            Rectangle::new(Point::new(x as i32, y as i32), Size::new(cell, cell))
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(&mut display)?;
        }
    }

    let buf = display.buffer();
    let zeros = buf.iter().filter(|&&b| b == 0x00).count();
    let ffs = buf.iter().filter(|&&b| b == 0xFF).count();
    tracing::debug!(
        buffer_len = buf.len(),
        zero_bytes = zeros,
        ff_bytes = ffs,
        "test pattern buffer"
    );

    tracing::info!("sending test pattern to EPD (V4 full refresh, ~2 s)");
    epd.display(&mut spi, display.buffer())
        .map_err(|e| anyhow::anyhow!("display failed: {e}"))?;
    tracing::info!("test pattern refresh complete");

    epd.sleep(&mut spi)
        .map_err(|e| anyhow::anyhow!("sleep failed: {e}"))?;

    Ok(())
}

/// Clears the display to white and puts it to sleep.
pub(crate) fn clear_display() -> anyhow::Result<()> {
    tracing::info!("clear_display: starting");
    let (mut spi, mut epd) = open_epd()?;

    tracing::info!("clearing EPD to white (V4 full refresh, ~2 s)");
    epd.clear(&mut spi, 0xFF)
        .map_err(|e| anyhow::anyhow!("clear failed: {e}"))?;
    tracing::info!("display cleared");

    epd.sleep(&mut spi)
        .map_err(|e| anyhow::anyhow!("sleep failed: {e}"))?;

    Ok(())
}
