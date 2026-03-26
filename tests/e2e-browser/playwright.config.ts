import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  globalSetup: "./global-setup.ts",
  testDir: "./tests",
  timeout: 60_000,
  // Retry flaky tests once. The yellow-blink detection in iroh-to-browser
  // depends on screenshot timing and occasionally misses transitions.
  retries: 1,
  // Run tests serially: each test starts its own relay process, and
  // parallel execution causes cargo lock contention + port conflicts.
  workers: 1,
  use: {
    ignoreHTTPSErrors: true,
    launchOptions: {
      args: [
        "--use-fake-device-for-media-stream",
        "--use-fake-ui-for-media-stream",
        // Accept self-signed certs for WebTransport in dev mode.
        "--ignore-certificate-errors",
      ],
    },
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  ],
});
