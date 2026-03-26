import { execSync } from "child_process";

/**
 * Pre-builds all Rust binaries and web assets that the e2e tests need.
 *
 * Without this, each test spawns `cargo run` concurrently, and they all
 * compete for the cargo lock — causing timeouts as processes block on
 * "Blocking waiting for file lock on artifact directory".
 *
 * Running `cargo build` once up front with all targets ensures the
 * binaries are ready and `cargo run` in each test starts instantly.
 */
export default function globalSetup() {
  console.log("Pre-building Rust binaries for e2e tests...");

  execSync("cargo make e2e-prebuild", {
    cwd: "../..",
    stdio: "inherit",
    timeout: 300_000, // 5 minutes for cold builds
  });

  // Rebuild relay web assets in case they're stale.
  execSync("npm run build", {
    cwd: "../../iroh-live-relay/web",
    stdio: "inherit",
    timeout: 60_000,
  });

  console.log("Rust binaries and web assets ready.");
}
