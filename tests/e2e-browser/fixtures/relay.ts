import { spawn, ChildProcess } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { findBinary } from "./bin";

export interface RelayInfo {
  process: ChildProcess;
  httpPort: number;
  irohAddr: string;
  dataDir: string;
}

/**
 * Starts the iroh-live-relay binary with --dev mode, binding to port 0
 * for both QUIC and HTTP so tests never collide with running services.
 *
 * Parses startup output to extract the actual bound ports and iroh endpoint ID.
 */
export async function startRelay(): Promise<RelayInfo> {
  const dataDir = mkdtempSync(join(tmpdir(), "iroh-live-relay-test-"));

  // Use the pre-built binary directly to avoid cargo recompilation during tests.
  const relayBin = findBinary("iroh-live-relay");

  const proc = spawn(
    relayBin,
    [
      "--bind",
      "[::]:0",
      "--http-bind",
      "[::]:0",
    ],
    {
      env: { ...process.env, IROH_LIVE_RELAY_DATA: dataDir },
      stdio: ["pipe", "pipe", "pipe"],
    }
  );

  const { httpPort, irohAddr } = await parseStartupOutput(proc);

  return { process: proc, httpPort, irohAddr, dataDir };
}

export async function stopRelay(relay: RelayInfo) {
  if (!relay?.process) return;
  relay.process.kill();
  // Wait for the process to exit so its ports are released before the
  // next test starts a new relay on port 0.
  await new Promise<void>((resolve) => {
    relay.process.on("exit", () => resolve());
    // Safety timeout in case the process doesn't exit cleanly.
    setTimeout(resolve, 5_000);
  });
  try {
    rmSync(relay.dataDir, { recursive: true, force: true });
  } catch {
    // Best effort cleanup.
  }
}

/**
 * Parses the relay's stderr for startup log lines to extract:
 * - HTTP port from "http listening" log
 * - QUIC port from "quic listening" log
 * - Iroh endpoint ID from "iroh endpoint bound" log
 */
function parseStartupOutput(
  proc: ChildProcess
): Promise<{ httpPort: number; irohAddr: string }> {
  return new Promise((resolve, reject) => {
    let httpPort: number | null = null;
    let irohAddr: string | null = null;
    let output = "";

    const timeout = setTimeout(() => {
      reject(
        new Error(
          `Relay did not start within 10s. Output:\n${output}`
        )
      );
    }, 10_000);

    const checkDone = () => {
      if (httpPort !== null && irohAddr !== null) {
        clearTimeout(timeout);
        resolve({ httpPort, irohAddr });
      }
    };

    const processLine = (line: string) => {
      output += line + "\n";

      // The relay prints plain "key: value" lines to stdout for parsing.
      const httpMatch = line.match(/^http port: (\d+)$/);
      if (httpMatch) {
        httpPort = parseInt(httpMatch[1], 10);
      }

      const irohMatch = line.match(/^iroh endpoint: ([a-f0-9]+)$/);
      if (irohMatch) {
        irohAddr = irohMatch[1];
      }

      checkDone();
    };

    // Relay tracing output may go to stdout or stderr depending on
    // tracing-subscriber configuration — listen on both.
    for (const stream of [proc.stdout, proc.stderr]) {
      stream?.on("data", (data: Buffer) => {
        const text = data.toString();
        for (const line of text.split("\n")) {
          if (line.trim()) processLine(line);
        }
      });
    }

    proc.on("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });

    proc.on("exit", (code) => {
      if (httpPort === null || irohAddr === null) {
        clearTimeout(timeout);
        reject(
          new Error(
            `Relay exited with code ${code} before startup completed. Output:\n${output}`
          )
        );
      }
    });
  });
}
