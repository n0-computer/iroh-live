import { spawn, ChildProcess } from "child_process";
import { appendFileSync, mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { AuthKey, writeAuthKey } from "./auth";
import { findBinary } from "./bin";

export interface RelayInfo {
  process: ChildProcess;
  httpPort: number;
  irohAddr: string;
  dataDir: string;
  /** Everything the relay printed so far, on stdout and stderr. */
  output: { text: string };
}

/** A private relay, and the key its tokens are signed with. */
export interface PrivateRelayInfo extends RelayInfo {
  key: AuthKey;
}

/**
 * Starts the iroh-live-relay binary on port 0 for both QUIC and HTTP, so
 * tests never collide with running services.
 *
 * Parses startup output to extract the actual bound ports and iroh endpoint ID.
 * The relay is open and pulls for anyone, since the tests pull tickets
 * without a token.
 */
export async function startRelay(): Promise<RelayInfo> {
  const dataDir = mkdtempSync(join(tmpdir(), "iroh-live-relay-test-"));
  return spawnRelay(dataDir, ["--open-pulls"]);
}

/**
 * Starts a private relay that verifies JWTs with a fresh key and has no
 * public area, so a session without a token is refused.
 *
 * The key lives in the relay's data directory, which `stopRelay` removes.
 */
export async function startPrivateRelay(): Promise<PrivateRelayInfo> {
  const dataDir = mkdtempSync(join(tmpdir(), "iroh-live-relay-test-"));
  const key = writeAuthKey(dataDir);
  const relay = await spawnRelay(dataDir, ["--auth-key", key.path]);
  return { ...relay, key };
}

/**
 * Waits until the relay prints a line matching `pattern` after `from`, an
 * offset into its output.
 */
export async function waitForRelayOutput(
  relay: RelayInfo,
  pattern: RegExp,
  from: number,
  timeoutMs = 15_000
): Promise<string> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const line = relay.output.text
      .slice(from)
      .split("\n")
      .find((line) => pattern.test(line));
    if (line) return line;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(
    `Relay did not print ${pattern} within ${timeoutMs}ms. Output since then:\n` +
      relay.output.text.slice(from)
  );
}

async function spawnRelay(dataDir: string, access: string[]): Promise<RelayInfo> {
  // Use the pre-built binary directly to avoid cargo recompilation during tests.
  const relayBin = findBinary("iroh-live-relay");

  const proc = spawn(
    relayBin,
    // No `--http-bind`: HTTP then shares the QUIC port, which the page connects to.
    ["--bind", "[::]:0", ...access],
    {
      env: { ...process.env, IROH_LIVE_RELAY_DATA: dataDir },
      stdio: ["pipe", "pipe", "pipe"],
    }
  );

  const output = { text: "" };
  const { httpPort, irohAddr } = await parseStartupOutput(proc, output);

  return { process: proc, httpPort, irohAddr, dataDir, output };
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
  proc: ChildProcess,
  all: { text: string }
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
        all.text += text;
        // `RELAY_LOG=/path npx playwright test` keeps the relay's own output.
        // Without it the harness reads these streams for the startup ports and
        // then discards everything, so a relay-side failure is invisible from
        // the test: a pull that could not resolve its ticket looked, from the
        // browser, like a page that simply never received an announce.
        if (process.env.RELAY_LOG) {
          appendFileSync(process.env.RELAY_LOG, text);
        }
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
