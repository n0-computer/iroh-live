import { test, expect } from "@playwright/test";
import { spawn, ChildProcess } from "child_process";
import { PNG } from "pngjs";
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
  // Force software H.264 to avoid hardware codec incompatibility with
  // the browser's JS decoder.
  const publisher = spawn("cargo", [
    "run",
    "-p", "iroh-live-cli",
    "--bin", "irl",
    "--",
    "publish",
    "--name", "pull-test",
    "--no-qr",
    "capture",
    "--video", "test",
    "--audio", "none",
    "--codec", "h264",
    "--video-presets", "360p",
  ]);

  // Wait for publisher to print its ticket.
  const ticket = await waitForTicket(publisher, 30_000);
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
  await expect(async () => {
    const hasContent = await canvas.evaluate((el: HTMLCanvasElement) => {
      if (el.width === 0 || el.height === 0) return false;
      const ctx = el.getContext("2d");
      if (!ctx) return false;
      const cx = Math.floor(el.width / 2);
      const cy = Math.floor(el.height / 2);
      const pixel = ctx.getImageData(cx, cy, 1, 1).data;
      return pixel[0] + pixel[1] + pixel[2] > 0;
    });
    expect(hasContent).toBe(true);
  }).toPass({ timeout: 30_000, intervals: [500] });

  // Verify live video by detecting the blinking yellow marker.
  const screenshots: Buffer[] = [];
  for (let i = 0; i < 40; i++) {
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

  if (!sawYellow || !sawNonYellow) {
    console.log(
      `Yellow detection failed. Center pixel colors: ${colors.join(" ")}`
    );
  }
  expect(sawYellow).toBe(true);
  expect(sawNonYellow).toBe(true);

  publisher.kill();
});

/**
 * Waits for the publisher to print "publishing at <ticket>" and extracts the ticket.
 */
function waitForTicket(
  proc: ChildProcess,
  timeoutMs: number
): Promise<string> {
  return new Promise((resolve, reject) => {
    let output = "";
    const prefix = "publishing at ";

    const timeout = setTimeout(() => {
      reject(
        new Error(
          `Timed out waiting for ticket after ${timeoutMs}ms. Output:\n${output}`
        )
      );
    }, timeoutMs);

    const check = (data: Buffer) => {
      const text = data.toString();
      output += text;
      const idx = output.indexOf(prefix);
      if (idx >= 0) {
        // Extract the ticket (everything after "publishing at " until newline).
        const start = idx + prefix.length;
        const end = output.indexOf("\n", start);
        const ticket = end >= 0 ? output.slice(start, end).trim() : output.slice(start).trim();
        if (ticket.length > 0) {
          clearTimeout(timeout);
          resolve(ticket);
        }
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
          `Process exited with code ${code} before ticket appeared. Output:\n${output}`
        )
      );
    });
  });
}

function analyzeCenter(
  pngBuffer: Buffer
): { isYellow: boolean; r: number; g: number; b: number } {
  const png = PNG.sync.read(pngBuffer);
  const cx = Math.floor(png.width / 2);
  const cy = Math.floor(png.height / 2);
  const idx = (cy * png.width + cx) * 4;
  const r = png.data[idx];
  const g = png.data[idx + 1];
  const b = png.data[idx + 2];

  return { isYellow: r > 180 && g > 180 && b < 100, r, g, b };
}
