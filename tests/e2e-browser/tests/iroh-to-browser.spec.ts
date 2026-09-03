import { test, expect } from "@playwright/test";
import { spawn, ChildProcess } from "child_process";
import { PNG } from "pngjs";
import { findBinary } from "../fixtures/bin";
import { startRelay, stopRelay, RelayInfo } from "../fixtures/relay";

const irlBin = findBinary("irl");

let relay: RelayInfo;

test.beforeAll(async () => {
  relay = await startRelay();
});

test.afterAll(async () => {
  await stopRelay(relay);
});

test("CLI publish → browser watch", async ({ page }) => {
  // Log browser console for debugging.
  page.on("console", (msg) => console.log(`BROWSER [${msg.type()}]: ${msg.text()}`));
  page.on("pageerror", (err) => console.log(`BROWSER ERROR: ${err}`));

  // Start publisher with test source, publishing to relay via iroh.
  // Force software H.264: workspace feature unification can activate
  // hardware codecs (V4L2, VAAPI) whose output the browser JS decoder
  // may not handle.
  const publisher = spawn(irlBin, [
    "publish",
    "--name", "hello",
    "--relay", relay.irohAddr,
    "--video", "test",
    "--audio", "none",
    "--codec", "h264",
    "--renditions", "360p",
  ]);

  // Wait for publisher to announce
  await waitForOutput(publisher, "publishing at", 30_000);

  // Navigate browser to relay watch page. HTTP and QUIC share the same
  // port (TCP vs UDP), so the moq-lite fingerprint flow works: it fetches
  // the fingerprint from http://host:port/certificate.sha256, then connects
  // via WebTransport to https://host:port/.
  const watchUrl = `http://localhost:${relay.httpPort}/?name=hello`;
  await page.goto(watchUrl);

  // Wait for canvas to be visible
  const canvas = page.locator("moq-watch canvas");
  await expect(canvas).toBeVisible({ timeout: 15_000 });

  // Wait for actual video content to render on the canvas.
  //
  // Sampled over a grid rather than at the centre pixel. The test pattern's
  // middle band is black except when the sweep bar crosses it, once every two
  // seconds and only a few pixels wide, so a centre probe was really waiting
  // for the bar to pass under one point and timed out when it did not.
  await expect(async () => {
    const hasContent = await canvas.evaluate((el: HTMLCanvasElement) => {
      if (el.width === 0 || el.height === 0) return false;
      const ctx = el.getContext("2d");
      if (!ctx) return false;
      const steps = 8;
      for (let row = 1; row < steps; row++) {
        for (let column = 1; column < steps; column++) {
          const x = Math.floor((el.width * column) / steps);
          const y = Math.floor((el.height * row) / steps);
          const pixel = ctx.getImageData(x, y, 1, 1).data;
          if (pixel[0] + pixel[1] + pixel[2] > 0) return true;
        }
      }
      return false;
    });
    expect(hasContent).toBe(true);
  }).toPass({ timeout: 20_000, intervals: [500] });

  // Verify live video by detecting the flashing yellow marker, which the test
  // pattern lights for 100ms once a second on the same media time as its audio
  // beep. 60 samples 100ms apart span six flashes.
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

  // The blinking marker should have appeared and disappeared at least once
  if (!sawYellow || !sawNonYellow) {
    console.log(`Yellow detection failed. Marker band colors: ${colors.join(" ")}`);
  }
  expect(sawYellow).toBe(true);
  expect(sawNonYellow).toBe(true);

  publisher.kill();
});

/**
 * Waits for a specific string to appear in stdout or stderr of a child process.
 */
function waitForOutput(
  proc: ChildProcess,
  needle: string,
  timeoutMs: number
): Promise<void> {
  return new Promise((resolve, reject) => {
    let output = "";

    const timeout = setTimeout(() => {
      reject(
        new Error(
          `Timed out waiting for "${needle}" after ${timeoutMs}ms. Output:\n${output}`
        )
      );
    }, timeoutMs);

    const check = (data: Buffer) => {
      const text = data.toString();
      output += text;
      if (output.includes(needle)) {
        clearTimeout(timeout);
        resolve();
      }
    };

    proc.stdout?.on("data", check);
    proc.stderr?.on("data", check);

    proc.on("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });

    proc.on("exit", (code) => {
      clearTimeout(timeout);
      reject(
        new Error(
          `Process exited with code ${code} before "${needle}" appeared. Output:\n${output}`
        )
      );
    });
  });
}

/**
 * Decodes a PNG screenshot and checks if the center pixel is yellow.
 * Yellow in the test pattern is (255, 255, 0). After codec compression
 * we use generous tolerance.
 */
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
 * See `moq-media/src/test_source/timing.rs` for the layout. The band runs from
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
