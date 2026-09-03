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

/// Pixels left clear on each side of the QR code.
const QR_MARGIN: u32 = 5;

/// Pixels reserved below the QR code for the label.
const LABEL_HEIGHT: usize = 14;

/// Where a QR code of a given module count is drawn on the panel, and how many
/// pixels each of its modules gets.
///
/// The scale is what decides whether a phone can read the panel at arm's
/// length, and it falls straight out of the payload: a shorter ticket needs
/// fewer modules, and fewer modules leave more pixels for each one. A ticket
/// that carries an endpoint id and a short broadcast name comes out at three
/// pixels per module on this 122 px panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QrLayout {
    /// Panel pixels per QR module.
    scale: usize,
    /// The drawn code's edge length in pixels.
    size_px: usize,
    x_offset: usize,
    y_offset: usize,
}

impl QrLayout {
    /// Returns the layout for a code `modules` wide, centred above the label.
    ///
    /// Scaled to the narrower panel dimension with a margin, and never below
    /// one pixel per module: a payload too long to fit is still worth drawing,
    /// because a code that overruns the panel is easier to diagnose than a
    /// blank one.
    fn centred(modules: usize) -> Self {
        let max_qr_px = (epd_v4::WIDTH - 2 * QR_MARGIN) as usize;
        let scale = std::cmp::max(1, max_qr_px / modules);
        let size_px = modules * scale;
        Self {
            scale,
            size_px,
            x_offset: (epd_v4::WIDTH as usize).saturating_sub(size_px) / 2,
            y_offset: (epd_v4::HEIGHT as usize - LABEL_HEIGHT).saturating_sub(size_px) / 2,
        }
    }
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

    let QrLayout {
        scale,
        size_px: qr_px,
        x_offset,
        y_offset,
    } = QrLayout::centred(modules);
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

#[cfg(test)]
mod tests {
    use qrcode::QrCode;

    use super::*;

    /// A ticket as `irl publish` and the pi-zero demo hand one out: the scheme,
    /// a base64url endpoint id, and a broadcast name.
    const TICKET: &str = "iroh-live:kX9mQ2vT7bL4nR8dY1sW6pA3zC5eH0jF2gK8uM4iO7Q/pi-zero";

    #[test]
    fn a_ticket_qr_gets_three_pixels_per_module() {
        // The reason the ticket carries an endpoint id and no addresses. A
        // ticket that listed the addresses of a multi-homed publisher ran to
        // 184 characters, which needs 57 modules and leaves one panel pixel
        // each. At 33 modules it gets three, and that is the difference between
        // a phone reading the panel first try and not at all.
        let code = QrCode::new(TICKET.as_bytes()).expect("a ticket fits in a QR code");
        assert_eq!(code.width(), 33);

        let layout = QrLayout::centred(code.width());
        assert_eq!(layout.scale, 3);
        assert_eq!(layout.size_px, 99);
    }

    /// The white border around the code is not spare room, it is the quiet zone
    /// a decoder needs to find the code at all, and the standard asks for four
    /// modules of it.
    ///
    /// This is the binding constraint on how big the modules can be, and it is
    /// easy to lose without noticing: a payload one QR version longer keeps the
    /// same three pixels per module, grows the code, and eats the border
    /// instead. The panel stays legible to the eye and stops scanning.
    #[test]
    fn a_ticket_qr_keeps_most_of_a_quiet_zone() {
        let code = QrCode::new(TICKET.as_bytes()).expect("a ticket fits in a QR code");
        let layout = QrLayout::centred(code.width());
        let quiet_modules = layout.x_offset as f64 / layout.scale as f64;
        assert!(
            quiet_modules >= 3.5,
            "only {quiet_modules:.1} modules of quiet zone around the code",
        );
    }

    /// Three pixels per module is the ceiling on this panel, not a compromise
    /// anyone can lift by tightening the margins.
    ///
    /// The narrow axis is 122 px and a ticket carrying a 32 byte endpoint id
    /// needs 33 modules, so a fourth pixel each would want 132 px and does not
    /// fit even with no border at all. Reaching four would mean 26 modules or
    /// fewer, which is a QR version holding 32 bytes, and the id alone is 43
    /// characters of base64.
    #[test]
    fn a_fourth_pixel_per_module_does_not_fit_the_panel() {
        let code = QrCode::new(TICKET.as_bytes()).expect("a ticket fits in a QR code");
        assert!(
            code.width() * 4 > epd_v4::WIDTH as usize,
            "four pixels per module would fit, so the layout is leaving some behind",
        );
    }

    #[test]
    fn a_qr_code_stays_on_the_panel() {
        for modules in [21, 25, 29, 33, 41, 53, 77] {
            let layout = QrLayout::centred(modules);
            assert!(
                layout.x_offset + layout.size_px <= epd_v4::WIDTH as usize,
                "{modules} modules overrun the panel width"
            );
            assert!(
                layout.y_offset + layout.size_px + LABEL_HEIGHT <= epd_v4::HEIGHT as usize,
                "{modules} modules overrun the panel height"
            );
        }
    }
}
