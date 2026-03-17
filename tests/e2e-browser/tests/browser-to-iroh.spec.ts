import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { startRelay, stopRelay, RelayInfo } from "../fixtures/relay";

let relay: RelayInfo;

test.beforeAll(async () => {
  relay = await startRelay();
});

test.afterAll(() => {
  stopRelay(relay);
});

test("Browser publish → CLI subscribe", async ({ page }) => {
  // Navigate to publish page with fake camera.
  // Pass the QUIC URL so <moq-publish> connects via WebTransport.
  const quicUrl = `https://localhost:${relay.quicPort}/`;
  const publishUrl = `http://localhost:${relay.httpPort}/publish.html?name=browser-stream&url=${encodeURIComponent(quicUrl)}`;
  await page.goto(publishUrl);

  // Wait for moq-publish element to be attached and publishing
  const publishEl = page.locator("moq-publish");
  await expect(publishEl).toBeAttached({ timeout: 10_000 });

  // Give the browser time to connect and start publishing
  await page.waitForTimeout(3000);

  // Subscribe from Rust side: connect to relay, receive 3 frames, exit 0
  const subscriber = spawn("cargo", [
    "run",
    "--example",
    "subscribe_test",
    "--",
    "--relay",
    relay.irohAddr,
    "--name",
    "browser-stream",
    "--frames",
    "3",
  ]);

  const { exitCode } = await waitForExit(subscriber, 30_000);
  expect(exitCode).toBe(0);
});

/**
 * Waits for a child process to exit, returning its exit code.
 */
function waitForExit(
  proc: ReturnType<typeof spawn>,
  timeoutMs: number
): Promise<{ exitCode: number | null }> {
  return new Promise((resolve, reject) => {
    let output = "";

    const timeout = setTimeout(() => {
      proc.kill();
      reject(
        new Error(
          `Process did not exit within ${timeoutMs}ms. Output:\n${output}`
        )
      );
    }, timeoutMs);

    proc.stdout?.on("data", (data: Buffer) => {
      output += data.toString();
    });
    proc.stderr?.on("data", (data: Buffer) => {
      output += data.toString();
    });

    proc.on("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });

    proc.on("exit", (code) => {
      clearTimeout(timeout);
      resolve({ exitCode: code });
    });
  });
}
