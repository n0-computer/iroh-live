import { expect, Locator } from "@playwright/test";
import { ChildProcess, spawn } from "child_process";
import { findBinary } from "./bin";

/**
 * Starts a standalone `irl publish` of the test source, which no relay knows
 * about until one pulls its ticket, and returns the process and the ticket.
 *
 * Forces software H.264 at 360p to avoid hardware codec incompatibility with
 * the browser's JS decoder.
 */
export async function startTicketPublisher(
  name: string
): Promise<{ publisher: ChildProcess; ticket: string }> {
  const publisher = spawn(findBinary("irl"), [
    "publish",
    "--name", name,
    "--no-qr",
    "--video", "test",
    "--audio", "none",
    "--codec", "h264",
    "--renditions", "360p",
  ]);
  try {
    const ticket = await waitForTicket(publisher, 30_000);
    return { publisher, ticket };
  } catch (err) {
    publisher.kill();
    throw err;
  }
}

/**
 * Waits for the publisher to print "publishing at <ticket>" and extracts the ticket.
 */
export function waitForTicket(
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

/**
 * Waits until the canvas shows video content, any non-black pixel.
 *
 * Sampled over a grid rather than at the centre pixel. The test pattern's
 * middle band is black except when the sweep bar crosses it, once every two
 * seconds and only a few pixels wide, so a centre probe was really waiting
 * for the bar to pass under one point and timed out when it did not.
 */
export async function expectVideoContent(canvas: Locator, timeoutMs = 30_000) {
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
  }).toPass({ timeout: timeoutMs, intervals: [500] });
}
