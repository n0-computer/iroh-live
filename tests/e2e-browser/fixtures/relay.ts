import { spawn, ChildProcess } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

export interface RelayInfo {
  process: ChildProcess;
  httpPort: number;
  quicPort: number;
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

  const proc = spawn(
    "cargo",
    [
      "run",
      "-p",
      "iroh-live-relay",
      "--",
      "--dev",
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

  const { httpPort, quicPort, irohAddr } = await parseStartupOutput(proc);

  return { process: proc, httpPort, quicPort, irohAddr, dataDir };
}

export function stopRelay(relay: RelayInfo) {
  relay.process.kill();
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
): Promise<{ httpPort: number; quicPort: number; irohAddr: string }> {
  return new Promise((resolve, reject) => {
    let httpPort: number | null = null;
    let quicPort: number | null = null;
    let irohAddr: string | null = null;
    let output = "";

    const timeout = setTimeout(() => {
      reject(
        new Error(
          `Relay did not start within 30s. Output:\n${output}`
        )
      );
    }, 30_000);

    const checkDone = () => {
      if (httpPort !== null && irohAddr !== null && quicPort !== null) {
        clearTimeout(timeout);
        resolve({ httpPort, quicPort, irohAddr });
      }
    };

    const processLine = (line: string) => {
      output += line + "\n";

      // Match: http_port=12345 (tracing structured field)
      const httpMatch = line.match(/http_port\s*=\s*(\d+)/);
      if (httpMatch) {
        httpPort = parseInt(httpMatch[1], 10);
      }

      // Match: endpoint_id=<hex> (tracing structured field)
      const irohMatch = line.match(/endpoint_id\s*=\s*([a-f0-9]+)/);
      if (irohMatch) {
        irohAddr = irohMatch[1];
      }

      // Match: bind=[::]:12345 or bind = [::]:12345
      const quicMatch = line.match(/bind\s*=\s*\[::\]:(\d+)/);
      if (quicMatch) {
        quicPort = parseInt(quicMatch[1], 10);
      }

      checkDone();
    };

    proc.stderr?.on("data", (data: Buffer) => {
      const text = data.toString();
      for (const line of text.split("\n")) {
        if (line.trim()) processLine(line);
      }
    });

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
