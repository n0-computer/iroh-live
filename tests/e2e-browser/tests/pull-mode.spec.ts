import { test, expect } from "@playwright/test";
import { PNG } from "pngjs";
import { expectVideoContent, startTicketPublisher } from "../fixtures/publisher";
import { startRelay, stopRelay, RelayInfo } from "../fixtures/relay";

let relay: RelayInfo;

test.beforeAll(async () => {
  relay = await startRelay();
});

test.afterAll(async () => {
  await stopRelay(relay);
});

/**
 * Pull mode: standalone publisher → relay pulls via ticket → browser watches.
 *
 * The publisher runs independently (NOT connected to the relay). The browser
 * navigates to the relay with the publisher's ticket as the broadcast name.
 * The relay detects the ticket, connects to the publisher via iroh, pulls the
 * broadcast, and serves it to the browser via WebTransport.
 */
test("pull mode: standalone publisher → relay → browser watch", async ({
  page,
}) => {
  // Log browser console for debugging.
  page.on("console", (msg) => console.log(`BROWSER [${msg.type()}]: ${msg.text()}`));
  page.on("pageerror", (err) => console.log(`BROWSER ERROR: ${err}`));

  // Start publisher with test source — no --relay flag, standalone P2P only.
  const { publisher, ticket } = await startTicketPublisher("pull-test");
  console.log(`Publisher ticket: ${ticket}`);

  // Navigate browser to relay watch page with the ticket as the name.
  // The relay will detect this is a ticket, pull the broadcast, and
  // serve it to the browser.
  const watchUrl = `http://localhost:${relay.httpPort}/?name=${encodeURIComponent(ticket)}`;
  await page.goto(watchUrl);

  // Wait for canvas to be visible.
  const canvas = page.locator("moq-watch canvas");
  await expect(canvas).toBeVisible({ timeout: 15_000 });

  // Wait for video content to render (non-black pixels).
  await expectVideoContent(canvas);

  // Verify live video by detecting the blinking yellow marker. The marker is
  // lit for 100ms once a second, so 60 samples 100ms apart span six flashes
  // rather than the four the previous 40 covered, which leaves margin for the
  // screenshot overhead stretching the interval.
  const screenshots: Buffer[] = [];
  for (let i = 0; i < 60; i++) {
    screenshots.push(await canvas.screenshot());
    await page.waitForTimeout(100);
  }

  let sawYellow = false;
  let sawNonYellow = false;
  const colors: string[] = [];

  for (const pngBuf of screenshots) {
    const { isYellow, r, g, b } = analyzeMarker(pngBuf);
    colors.push(`(${r},${g},${b})`);
    if (isYellow) sawYellow = true;
    else sawNonYellow = true;
  }

  if (!sawYellow || !sawNonYellow) {
    console.log(
      `Yellow detection failed. Marker band colors: ${colors.join(" ")}`
    );
  }
  expect(sawYellow).toBe(true);
  expect(sawNonYellow).toBe(true);

  publisher.kill();
});

/**
 * Looks for the test pattern's flashing marker, which is yellow and is the only
 * yellow thing in the frame.
 *
 * The marker is the bottom band rather than the centre, so this samples across
 * that band rather than one pixel in the middle of it: the sweep bar crosses the
 * band and would occlude a single fixed sample every time it passed. Any yellow
 * sample counts, since the bar is a thin vertical slice and cannot cover them
 * all at once.
 *
 * See `iroh-live-media/src/test_source/timing.rs` for the layout. The band runs from
 * thirteen sixteenths of the height to the bottom, so nine tenths is inside it
 * with room either side for the scaling the browser applies.
 */
function analyzeMarker(pngBuffer: Buffer): { isYellow: boolean; r: number; g: number; b: number } {
  const png = PNG.sync.read(pngBuffer);
  const y = Math.floor(png.height * 0.9);
  let sample = { isYellow: false, r: 0, g: 0, b: 0 };
  for (const fraction of [0.1, 0.3, 0.5, 0.7, 0.9]) {
    const x = Math.floor(png.width * fraction);
    const idx = (y * png.width + x) * 4;
    const r = png.data[idx];
    const g = png.data[idx + 1];
    const b = png.data[idx + 2];
    // High red, high green, low blue, with tolerance for codec artifacts and
    // for the browser's scaling.
    if (r > 180 && g > 180 && b < 100) {
      return { isYellow: true, r, g, b };
    }
    if (fraction === 0.5) {
      sample = { isYellow: false, r, g, b };
    }
  }
  return sample;
}
