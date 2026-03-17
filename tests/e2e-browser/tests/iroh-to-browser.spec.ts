import { test, expect } from "@playwright/test";
import { spawn, ChildProcess } from "child_process";
import { PNG } from "pngjs";
import { startRelay, stopRelay, RelayInfo } from "../fixtures/relay";

let relay: RelayInfo;

test.beforeAll(async () => {
  relay = await startRelay();
});

test.afterAll(() => {
  stopRelay(relay);
});

test("CLI publish → browser watch", async ({ page }) => {
  // Start publisher with test source, publishing to relay via iroh
  const publisher = spawn("cargo", [
    "run",
    "--example",
    "publish",
    "--",
    "--test-source",
    "--no-audio",
    "--relay",
    relay.irohAddr,
    "--name",
    "hello",
    "--video-presets",
    "P360",
  ]);

  // Wait for publisher to announce
  await waitForOutput(publisher, "publishing at", 30_000);

  // Navigate browser to relay watch page.
  // The watch page loads from the HTTP server, but <moq-watch> connects
  // via WebTransport to the QUIC port. We pass the QUIC URL explicitly.
  const quicUrl = `https://localhost:${relay.quicPort}/`;
  const watchUrl = `http://localhost:${relay.httpPort}/?name=hello&url=${encodeURIComponent(quicUrl)}`;
  await page.goto(watchUrl);

  // Wait for canvas to be visible
  const canvas = page.locator("moq-watch canvas");
  await expect(canvas).toBeVisible({ timeout: 15_000 });

  // Give the video pipeline time to start producing frames
  await page.waitForTimeout(3000);

  // Verify canvas has non-zero dimensions (video is rendering)
  const size = await canvas.evaluate((el: HTMLCanvasElement) => ({
    w: el.width,
    h: el.height,
  }));
  expect(size.w).toBeGreaterThan(0);
  expect(size.h).toBeGreaterThan(0);

  // Verify live video by detecting the blinking yellow marker.
  // The test pattern has a centered yellow square that blinks every 15 frames
  // (0.5s at 30fps). We capture screenshots every 100ms for 3s and check
  // that the center pixel alternates between yellow and non-yellow.
  const screenshots: Buffer[] = [];
  for (let i = 0; i < 30; i++) {
    screenshots.push(await canvas.screenshot());
    await page.waitForTimeout(100);
  }

  let sawYellow = false;
  let sawNonYellow = false;

  for (const pngBuf of screenshots) {
    const isYellow = analyzeCenter(pngBuf);
    if (isYellow) sawYellow = true;
    else sawNonYellow = true;
  }

  // The blinking marker should have appeared and disappeared at least once
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
function analyzeCenter(pngBuffer: Buffer): boolean {
  const png = PNG.sync.read(pngBuffer);
  const cx = Math.floor(png.width / 2);
  const cy = Math.floor(png.height / 2);
  const idx = (cy * png.width + cx) * 4;
  const r = png.data[idx];
  const g = png.data[idx + 1];
  const b = png.data[idx + 2];

  // Yellow: high R, high G, low B (with tolerance for codec artifacts)
  return r > 180 && g > 180 && b < 100;
}
