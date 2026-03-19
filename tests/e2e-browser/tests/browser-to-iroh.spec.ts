import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { startRelay, stopRelay, RelayInfo } from "../fixtures/relay";

let relay: RelayInfo;

test.beforeAll(async () => {
  relay = await startRelay();
});

test.afterAll(async () => {
  await stopRelay(relay);
});

test("Browser publish → CLI subscribe", async ({ page }) => {
  // Log browser console for debugging.
  page.on("console", (msg) => console.log(`BROWSER [${msg.type()}]: ${msg.text()}`));
  page.on("pageerror", (err) => console.log(`BROWSER ERROR: ${err}`));

  // Navigate to publish page with fake camera. HTTP and QUIC share the
  // same port, so the moq-lite fingerprint flow works automatically.
  const publishUrl = `http://localhost:${relay.httpPort}/publish.html?name=browser-stream`;
  await page.goto(publishUrl);

  // Wait for moq-publish element to be attached and publishing
  const publishEl = page.locator("moq-publish");
  await expect(publishEl).toBeAttached({ timeout: 10_000 });

  // Wait for the browser to actually publish tracks (not just connect).
  // The subscribe_test retries if the catalog isn't available yet, but
  // we need the browser to have published before starting the subscriber.
  await page.waitForTimeout(5000);

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
  ], {
    env: { ...process.env, RUST_LOG: "info" },
  });

  const result = await waitForExit(subscriber, 45_000);
  if (result.exitCode !== 0) {
    throw new Error(`subscribe_test exited with code ${result.exitCode}. Output:\n${result.output}`);
  }
});

/**
 * Waits for a child process to exit, returning its exit code.
 */
function waitForExit(
  proc: ReturnType<typeof spawn>,
  timeoutMs: number
): Promise<{ exitCode: number | null; output: string }> {
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
      resolve({ exitCode: code, output });
    });
  });
}
