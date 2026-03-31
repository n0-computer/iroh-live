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
    "--video-presets", "360p",
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
  // Poll until the canvas buffer has non-zero dimensions and non-black pixels.
  await expect(async () => {
    const hasContent = await canvas.evaluate((el: HTMLCanvasElement) => {
      if (el.width === 0 || el.height === 0) return false;
      const ctx = el.getContext("2d");
      if (!ctx) return false;
      // Sample center pixel
      const cx = Math.floor(el.width / 2);
      const cy = Math.floor(el.height / 2);
      const pixel = ctx.getImageData(cx, cy, 1, 1).data;
      // Any non-black pixel means video is rendering
      return pixel[0] + pixel[1] + pixel[2] > 0;
    });
    expect(hasContent).toBe(true);
  }).toPass({ timeout: 20_000, intervals: [500] });

  // Verify live video by detecting the blinking yellow marker.
  // The test pattern has a centered yellow square that blinks every 15 frames
  // (0.5s at 30fps). We capture screenshots every 100ms for 4s and check
  // that the center pixel alternates between yellow and non-yellow.
  const screenshots: Buffer[] = [];
  // Capture for 6s at 100ms intervals. The test pattern blinks every 500ms,
  // so 60 samples gives ~12 full blink cycles even with codec latency.
  for (let i = 0; i < 60; i++) {
    screenshots.push(await canvas.screenshot());
    await page.waitForTimeout(100);
  }

  let sawYellow = false;
  let sawNonYellow = false;
  const colors: string[] = [];

  for (const pngBuf of screenshots) {
    const { isYellow, r, g, b } = analyzeCenter(pngBuf);
    colors.push(`(${r},${g},${b})`);
    if (isYellow) sawYellow = true;
    else sawNonYellow = true;
  }

  // The blinking marker should have appeared and disappeared at least once
  if (!sawYellow || !sawNonYellow) {
    console.log(`Yellow detection failed. Center pixel colors: ${colors.join(" ")}`);
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
function analyzeCenter(pngBuffer: Buffer): { isYellow: boolean; r: number; g: number; b: number } {
  const png = PNG.sync.read(pngBuffer);
  const cx = Math.floor(png.width / 2);
  const cy = Math.floor(png.height / 2);
  const idx = (cy * png.width + cx) * 4;
  const r = png.data[idx];
  const g = png.data[idx + 1];
  const b = png.data[idx + 2];

  // Yellow: high R, high G, low B (with tolerance for codec artifacts)
  return { isYellow: r > 180 && g > 180 && b < 100, r, g, b };
}
