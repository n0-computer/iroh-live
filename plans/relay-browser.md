# Relay + Browser E2E Plan

Goal: `iroh-live-relay` binary that bridges iroh-live publishers to browser
viewers (and vice versa) via moq-relay, with Playwright E2E tests. Clean,
concise, production-shaped.

## Architecture

```
iroh-live publish ──(iroh P2P)──→ iroh-live-relay ←──(WebTransport/H3 via noq)── browser <moq-watch>
browser <moq-publish> ──(WebTransport/H3)──→ iroh-live-relay ──(iroh P2P)──→ iroh-live subscribe
```

The relay:
1. Runs moq-relay as library (**noq** backend, never quinn) with iroh + websocket
2. Accepts iroh P2P (moq-lite ALPN) **and** WebTransport/H3 (noq) connections
3. Bridges both into a shared `moq-lite::Origin` (already built into moq-relay)
4. Serves a built web app with `<moq-watch>` and `<moq-publish>` web components
5. In production: ACME auto-cert (same cert for QUIC + HTTPS)
6. In dev: self-signed certs via moq-native's `--tls-generate`

## Existing pieces

| Component | Location | Status |
|-----------|----------|--------|
| moq-relay lib | `../moq/rs/moq-relay` (HEAD `91d13763`) | Ready. Exports `Config`, `Web`, `WebState`, `Cluster`, `Connection`, `Auth`. Features: `noq`, `iroh`, `websocket`. |
| moq-native | `../moq/rs/moq-native` | Ready. `ServerConfig`, `ServeCerts`, `IrohEndpointConfig`, noq client/server, TLS fingerprint. |
| iroh-moq | `iroh-moq/src/lib.rs` | Ready. `Moq`, `MoqSession`, `MoqProtocolHandler`. P2P via `Endpoint::connect(addr, ALPN)`. |
| iroh-live | `iroh-live/src/live.rs` | Ready. `Live` with `publish()`, `subscribe()`. No relay URL support yet. |
| `@moq/watch` | npm v0.2.3 | `<moq-watch url name paused volume muted reload jitter>`. WebTransport + WebCodecs. Canvas or video child. |
| `@moq/publish` | npm v0.2.3 | `<moq-publish url name muted invisible source>`. Camera/screen/file. WebCodecs encode. |
| `@moq/lite` | npm v0.1.5 | Core transport. `Moq.Connection.Reload` handles reconnect. |
| `@moq/hang` | npm v0.2.0 | Catalog + container layer for WebCodecs. |
| Test sources | `moq-media/src/test_util.rs` | `TestVideoSource`, `TestAudioSource`, `SineAudioSource`. |
| moq demo | `../moq/js/demo/` | Reference Vite app using workspace `@moq/*` packages. Shows exact usage patterns. |

## Key insight: no custom bridge needed

moq-relay already bridges all transport backends through a shared origin:
- `server.accept()` loop handles noq, quinn, iroh — each yields a `Request`
- `Connection::run()` routes through `Cluster` which manages `OriginProducer`/`OriginConsumer`
- A broadcast published via iroh is automatically visible to WebTransport subscribers

**We just configure moq-relay with `noq` + `iroh` features enabled.**

## Step-by-step implementation

### Step 1: Web app (build step, not CDN)

Create `iroh-live-relay/web/` as a small Vite project:

```
iroh-live-relay/web/
  package.json          # deps: @moq/watch, @moq/publish, vite, solid-js, solid-element
  vite.config.ts        # single-page app build
  src/
    index.html          # watch page
    publish.html        # publish page
    index.ts            # watch entry (imports @moq/watch/element)
    publish.ts           # publish entry (imports @moq/publish/element)
```

**package.json** (minimal):
```json
{
  "name": "iroh-live-relay-web",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "vite --open",
    "build": "vite build"
  },
  "dependencies": {
    "@moq/watch": "^0.2.3",
    "@moq/publish": "^0.2.3"
  },
  "devDependencies": {
    "solid-js": "^1.9.10",
    "solid-element": "^1.9.1",
    "vite": "^7.3.1",
    "vite-plugin-solid": "^2.11.10",
    "typescript": "^5.9.2"
  }
}
```

Note: `solid-js` and `solid-element` are needed because the published `@moq/watch`
and `@moq/publish` packages depend on them at build time (they use Solid.js
internally for their web components). The `../moq/js/demo/package.json` confirms
this pattern — it lists them as devDependencies with the comment "needed because
Vite resolves raw .tsx source from workspace packages".

The *published* npm packages do not include pre-built dist — they export raw
`.ts`/`.tsx` source entries (`"./element": "./src/element.ts"`). Solid + Vite
are required even for published packages. The vite-plugin-solid transpiles the
JSX at build time.

**index.html** (watch page):
```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>iroh-live</title>
</head>
<body>
  <h1>Watch</h1>
  <moq-watch id="watch" muted reload jitter="100">
    <canvas style="width: 100%; height: auto;"></canvas>
  </moq-watch>
  <p><a href="publish.html">Publish a broadcast</a></p>
  <script type="module" src="index.ts"></script>
</body>
</html>
```

**index.ts**:
```typescript
import "@moq/watch/element";
const watch = document.getElementById("watch")!;
const params = new URLSearchParams(window.location.search);
const name = params.get("name") ?? "hello";
// Auto-detect relay URL from page origin (same host, HTTPS for WebTransport)
const url = params.get("url") ?? `${window.location.origin}/`;
watch.setAttribute("url", url);
watch.setAttribute("name", name);
```

**publish.html** + **publish.ts**: same pattern with `<moq-publish source="camera">`.

**Build step**: `cd iroh-live-relay/web && bun install && bun run build` → outputs
to `iroh-live-relay/web/dist/`. The relay binary embeds this via
`include_dir!("web/dist")` (using the `include_dir` crate) or serves from disk.

### Step 2: Create `iroh-live-relay` crate

New workspace member: `iroh-live-relay/`

**`Cargo.toml`**:
```toml
[package]
name = "iroh-live-relay"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "iroh-live-relay"
path = "src/main.rs"

[dependencies]
moq-relay = { git = "https://github.com/moq-dev/moq.git", branch = "main", default-features = false, features = ["noq", "iroh", "websocket"] }
moq-native = { git = "https://github.com/moq-dev/moq.git", branch = "main", default-features = false, features = ["noq", "iroh", "aws-lc-rs"] }
axum = { version = "0.8", features = ["tokio"] }
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
rustls = { version = "0.23", features = ["aws-lc-rs"], default-features = false }
include_dir = "0.7"
dirs = "6"
```

#### `RelayServer` struct

The relay wraps its state in a `RelayServer` that owns a persistent data
directory for certs and the iroh secret key. Two constructors:

- `RelayServer::new(path)` — takes an explicit data directory path
- `RelayServer::from_env()` — reads `IROH_LIVE_RELAY_DATA` env var; falls back
  to `dirs::data_dir() / "iroh-live-relay"`

On first run the directory is created. The iroh secret key is generated and
written to `data_dir/iroh_secret_key`. On subsequent runs, the existing key is
loaded so the iroh endpoint ID stays stable. TLS certs (self-signed or ACME)
are stored under `data_dir/certs/`.

**`src/main.rs`** structure:
```rust
use moq_relay::*;
use std::{path::PathBuf, sync::Arc};

/// Persistent data directory for certs, iroh key, and relay state.
struct RelayServer {
    data_dir: PathBuf,
}

impl RelayServer {
    /// Creates a relay with an explicit data directory.
    fn new(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let data_dir = path.into();
        std::fs::create_dir_all(&data_dir)?;
        Ok(Self { data_dir })
    }

    /// Creates a relay using `IROH_LIVE_RELAY_DATA` env var, or the
    /// platform data directory (`~/.local/share/iroh-live-relay` on Linux).
    fn from_env() -> anyhow::Result<Self> {
        let path = match std::env::var("IROH_LIVE_RELAY_DATA") {
            Ok(p) => PathBuf::from(p),
            Err(_) => dirs::data_dir()
                .expect("no platform data directory")
                .join("iroh-live-relay"),
        };
        Self::new(path)
    }

    fn iroh_secret_key_path(&self) -> PathBuf {
        self.data_dir.join("iroh_secret_key")
    }

    fn certs_dir(&self) -> PathBuf {
        self.data_dir.join("certs")
    }

    /// Loads or generates the iroh secret key.
    fn iroh_secret_key(&self) -> anyhow::Result<iroh::SecretKey> {
        let path = self.iroh_secret_key_path();
        if path.exists() {
            let hex = std::fs::read_to_string(&path)?;
            Ok(hex.trim().parse()?)
        } else {
            let key = iroh::SecretKey::generate(&mut rand::rng());
            let hex = data_encoding::HEXLOWER.encode(&key.to_bytes());
            std::fs::write(&path, &hex)?;
            Ok(key)
        }
    }
}

#[derive(clap::Parser)]
struct Cli {
    /// Dev mode: self-signed certs, HTTP fingerprint endpoint
    #[arg(long)]
    dev: bool,
    /// ACME domain for automatic cert provisioning
    #[arg(long)]
    acme_domain: Option<String>,
    /// Bind address for QUIC (noq) + iroh
    #[arg(long, default_value = "[::]:4443")]
    bind: std::net::SocketAddr,
    /// Bind address for HTTP (fingerprint, static files, websocket)
    #[arg(long, default_value = "[::]:4443")]
    http_bind: std::net::SocketAddr,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install crypto provider");

    let cli = Cli::parse();
    let relay = RelayServer::from_env()?;
    let secret_key = relay.iroh_secret_key()?;

    let mut config = Config::default();

    // Server: noq backend, bind address, TLS
    config.server.bind = Some(cli.bind);
    config.server.backend = Some(moq_native::QuicBackend::Noq);
    if cli.dev {
        config.server.tls.generate = vec!["localhost".to_string()];
    } else if let Some(ref domain) = cli.acme_domain {
        // ACME: provision cert into relay.certs_dir(), set paths
        // (see Step 7)
        todo!("ACME provisioning");
    }

    // Auth: public access (no JWT required)
    config.auth = AuthConfig { public: Some("".to_string()), ..Default::default() };

    // Web: HTTP listener for fingerprint + static files + websocket
    config.web.http.listen = Some(cli.http_bind);
    config.web.ws = true;

    // Iroh: enable with persistent secret key
    config.iroh = moq_native::IrohEndpointConfig {
        enabled: Some(true),
        secret: Some(secret_key),
        ..Default::default()
    };

    // Initialize server + client + iroh
    let mut server = config.server.clone().init()?;
    let client = config.client.clone().init()?;
    let iroh = config.iroh.clone().bind().await?;
    let (mut server, client) = (server.with_iroh(iroh.clone()), client.with_iroh(iroh.clone()));

    if let Some(ref iroh_ep) = iroh {
        tracing::info!(endpoint_id = %iroh_ep.endpoint.id(), "iroh endpoint bound");
    }

    let auth = config.auth.init().await?;
    let cluster = Cluster::new(config.cluster, client);
    tokio::spawn({ let c = cluster.clone(); async move { c.run().await.expect("cluster failed") }});

    // Web server: fork Web::run() to inject static file routes alongside
    // relay routes (fingerprint, announced, websocket).
    let state = Arc::new(WebState {
        auth: auth.clone(),
        cluster: cluster.clone(),
        tls_info: server.tls_info(),
        conn_id: Default::default(),
    });

    // Build router with static files + relay routes + websocket fallback
    let router = build_router(state, config.web);
    let http_listener = tokio::net::TcpListener::bind(cli.http_bind).await?;
    let http_port = http_listener.local_addr()?.port();
    tracing::info!(http_port, "http listening");

    tokio::spawn(async move {
        axum::serve(http_listener, router).await.expect("http server failed");
    });

    tracing::info!(bind = %cli.bind, "listening");

    // Accept loop (same as moq-relay main.rs)
    let mut conn_id = 0u64;
    while let Some(request) = server.accept().await {
        let conn = Connection {
            id: conn_id,
            request,
            cluster: cluster.clone(),
            auth: auth.clone(),
        };
        conn_id += 1;
        tokio::spawn(async move {
            if let Err(err) = conn.run().await {
                tracing::warn!(%err, "connection closed");
            }
        });
    }
    Ok(())
}
```

**Static file serving**: fork `Web::run()` in our main.rs. Since `Web` is ~60
lines, we replicate the router setup with our static routes injected. This
gives us moq-relay's `/certificate.sha256`, `/announced/*`, and websocket
handler, plus our own static file routes from `web/dist/`.

```rust
use include_dir::{include_dir, Dir};
static WEB_DIR: Dir = include_dir!("web/dist");

fn static_routes() -> axum::Router<Arc<WebState>> {
    axum::Router::new()
        .route("/", get(|| async { serve_static("index.html") }))
        .route("/{*path}", get(|path| async move { serve_static(&path) }))
}
```

We build our own axum Router combining moq-relay's routes + our static file
routes + websocket fallback. Reference: `../moq/rs/moq-relay/src/web.rs`
lines 56-95.

### Step 3: Publish to relay from CLI

Modify `iroh-live/examples/publish.rs`.

Add CLI flags:
```rust
#[derive(Parser)]
struct Cli {
    // ... existing flags ...
    /// Use test video source (no camera needed)
    #[arg(long)]
    test_source: bool,
    /// Relay's iroh endpoint address (additionally publishes to relay)
    #[arg(long)]
    relay: Option<String>,
}
```

**Test source mode**: when `--test-source`, use `TestVideoSource::new(w, h)` from
`moq-media::test_util` instead of `CameraCapturer`. Need to add `test-util` as
a dev-dependency or conditional dep for the example.

**Relay publish is additive**: the publisher always calls `live.publish()` for
P2P availability. When `--relay` is set, it *additionally* connects to the
relay and publishes the same broadcast there. Both paths run simultaneously.

```rust
// Always publish P2P
live.publish(name, &broadcast).await?;

// Additionally push to relay if specified
if let Some(relay_addr) = cli.relay {
    let addr: EndpointAddr = relay_addr.parse()?;
    let session = live.transport().connect(addr).await?;
    session.publish_broadcast("hello", broadcast.producer());
    tracing::info!(%relay_addr, "published to relay");
}
```

`MoqSession` doesn't currently expose a way to publish a broadcast — the
`publish: OriginProducer` field is private. We add `publish_broadcast()` in
Step 6.

### Step 4: Subscribe from relay in CLI

For the browser-to-iroh test, the CLI subscribes to a broadcast published by a
browser through the relay. This works with existing `live.subscribe()` — connect
to relay's iroh endpoint, subscribe to the broadcast name. The relay's Cluster
serves it from the WebTransport publisher.

```rust
let (session, remote) = live.subscribe(relay_iroh_addr, "browser-stream").await?;
let mut track = remote.video_ready().await?;
```

### Step 5: Playwright E2E tests

```
tests/e2e-browser/
  package.json
  playwright.config.ts
  tests/
    iroh-to-browser.spec.ts
    browser-to-iroh.spec.ts
  fixtures/
    relay.ts              # manages relay process lifecycle
    publisher.ts          # manages publish example process
```

**package.json**:
```json
{
  "name": "iroh-live-e2e",
  "private": true,
  "scripts": {
    "test": "playwright test",
    "test:headed": "playwright test --headed"
  },
  "devDependencies": {
    "@playwright/test": "^1.50.0"
  }
}
```

**playwright.config.ts**:
```typescript
import { defineConfig, devices } from '@playwright/test';
export default defineConfig({
  testDir: './tests',
  timeout: 30_000,
  use: {
    // Self-signed certs in dev mode
    ignoreHTTPSErrors: true,
    launchOptions: {
      args: [
        '--use-fake-device-for-media-stream',
        '--use-fake-ui-for-media-stream',
        // Accept self-signed cert for WebTransport
        '--ignore-certificate-errors',
        '--origin-to-force-quic-on=localhost:4443',
      ],
    },
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
  ],
});
```

**Relay fixture** (`fixtures/relay.ts`):

The fixture launches the relay in a **tempdir** to isolate test state from the
user's real data directory. It uses `IROH_LIVE_RELAY_DATA` env var to point at
the tempdir, and binds to **port 0** for both QUIC and HTTP so tests never
collide with running services. The actual HTTP port is parsed from the relay's
stdout (the `http_port` field in the `"http listening"` log line).

```typescript
import { spawn, ChildProcess } from 'child_process';
import { mkdtempSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';

export class RelayFixture {
  process: ChildProcess;
  httpPort: number;
  irohAddr: string;
  dataDir: string;

  static async start(): Promise<RelayFixture> {
    const dataDir = mkdtempSync(join(tmpdir(), 'iroh-live-relay-test-'));

    const proc = spawn('cargo', [
      'run', '-p', 'iroh-live-relay', '--',
      '--dev',
      '--bind', '[::]:0',
      '--http-bind', '[::]:0',
    ], {
      env: { ...process.env, IROH_LIVE_RELAY_DATA: dataDir },
    });

    // Parse port and iroh addr from stdout
    const { httpPort, irohAddr } = await parseStartupOutput(proc);
    return { process: proc, httpPort, irohAddr, dataDir };
  }

  async stop() {
    this.process.kill();
    // Clean up tempdir
  }
}
```

**Test: iroh-to-browser** (`tests/iroh-to-browser.spec.ts`):
```typescript
test('CLI publish → browser watch', async ({ page }) => {
  // 1. Relay is running (from fixture)
  // 2. Start publisher: cargo run --example publish -- --test-source --relay <iroh-addr>
  const publisher = spawn('cargo', ['run', '--example', 'publish', '--',
    '--test-source', '--relay', relay.irohAddr]);

  // 3. Wait for publisher to announce
  await waitForOutput(publisher, 'publishing at');

  // 4. Navigate browser to relay watch page
  await page.goto(`http://localhost:${relay.httpPort}/?name=hello`);

  // 5. Wait for canvas to have content
  const canvas = page.locator('moq-watch canvas');
  await expect(canvas).toBeVisible({ timeout: 10_000 });

  // 6. Verify video is actually playing (canvas has non-zero size)
  const size = await canvas.evaluate(el => ({
    w: el.width, h: el.height
  }));
  expect(size.w).toBeGreaterThan(0);
  expect(size.h).toBeGreaterThan(0);

  // 7. Verify the video content is correct by detecting the blinking marker.
  //    The test video source renders a large yellow square (centered, ~25% of
  //    frame area) that blinks on/off every 15 frames. We capture screenshots
  //    every 50ms for 2s and check that the center pixel alternates between
  //    yellow and non-yellow, proving live video is actually playing.
  const screenshots: Buffer[] = [];
  for (let i = 0; i < 40; i++) {
    screenshots.push(await canvas.screenshot());
    await page.waitForTimeout(50);
  }

  // Analyze center pixel of each screenshot for yellow presence.
  // Yellow in the test pattern is (255, 255, 0) — after codec compression
  // we allow generous tolerance.
  let sawYellow = false;
  let sawNonYellow = false;
  for (const png of screenshots) {
    const isYellow = await analyzeCenter(png); // decode PNG, check center pixel
    if (isYellow) sawYellow = true;
    else sawNonYellow = true;
  }
  expect(sawYellow).toBe(true);    // marker appeared at least once
  expect(sawNonYellow).toBe(true); // marker disappeared at least once

  publisher.kill();
});
```

This proves the video pipeline is working end-to-end: the test source generates
frames with a known marker, the codec compresses them, iroh-moq transports them
through the relay, the browser decodes and renders them, and we detect the
expected content changes over time.

**Prerequisite — enlarge the test pattern marker**: the current
`make_test_pattern()` in `rusty-codecs/src/codec/test_util.rs` draws SMPTE bars
with a thin moving white stripe. We add a large, centered, blinking yellow
square: roughly 25% of the frame area, toggling on/off every 15 frames (0.5s at
30fps). This makes it robust to compression artifacts and easy to detect in
screenshots.

```rust
// In make_test_pattern(), after the SMPTE bars:
// Blinking yellow marker: on for 15 frames, off for 15 frames
let blink_on = (frame_index / 15) % 2 == 0;
if blink_on {
    let sq_w = w / 2;
    let sq_h = h / 2;
    let x0 = (w - sq_w) / 2;
    let y0 = (h - sq_h) / 2;
    for y in y0..(y0 + sq_h) {
        for x in x0..(x0 + sq_w) {
            let offset = ((y * w + x) * 4) as usize;
            raw[offset] = 255;     // R
            raw[offset + 1] = 255; // G
            raw[offset + 2] = 0;   // B
            raw[offset + 3] = 255; // A
        }
    }
}
```

**Test: browser-to-iroh** (`tests/browser-to-iroh.spec.ts`):
```typescript
test('Browser publish → CLI subscribe', async ({ page }) => {
  // 1. Navigate to publish page with fake camera
  await page.goto(`http://localhost:${relay.httpPort}/publish.html?name=browser-stream`);

  // 2. Wait for publish to start (moq-publish element connected)
  await page.locator('moq-publish').waitFor({ state: 'attached' });
  // Wait a bit for the broadcast to reach the relay
  await page.waitForTimeout(2000);

  // 3. Subscribe from Rust side
  //    Use a small helper binary or the watch example with --relay
  const subscriber = spawn('cargo', ['run', '--example', 'subscribe-test', '--',
    '--relay', relay.irohAddr, '--name', 'browser-stream', '--frames', '3']);

  // 4. Wait for subscriber to receive frames
  const { exitCode } = await waitForExit(subscriber, 15_000);
  expect(exitCode).toBe(0);
});
```

For the browser-to-iroh test, we need a small subscribe helper (or extend an
existing example) that connects to relay, subscribes, waits for N frames, exits 0.

### Step 6: Iroh-moq changes

Add to `MoqSession` in `iroh-moq/src/lib.rs`:

```rust
impl MoqSession {
    /// Publishes a broadcast to the remote peer (e.g. relay).
    pub fn publish_broadcast(&self, name: impl ToString, producer: BroadcastProducer) {
        self.publish.insert(name.to_string(), producer);
    }

    /// Returns the origin producer for advanced publish operations.
    pub fn origin_producer(&self) -> &OriginProducer {
        &self.publish
    }

    /// Returns the origin consumer for advanced subscribe operations.
    pub fn origin_consumer(&self) -> &OriginConsumer {
        &self.subscribe
    }
}
```

### Step 7: ACME cert provisioning

Automatic TLS via ACME HTTP-01, sharing one cert between HTTPS and QUIC.

#### Network topology

```
--http-bind (:8080) TCP  → plain HTTP   → content + ACME HTTP-01 challenges
                                         → after cert ready: 301 → HTTPS (except /.well-known/acme-challenge/)
--bind (:4443) UDP       → QUIC/WebTransport (existing, same ACME cert)
--bind (:4443) TCP       → HTTPS        → same content as HTTP
```

HTTPS reuses the `--bind` address over TCP. QUIC uses the same address over
UDP. Both share the ACME-provisioned certificate.

#### CLI additions

```rust
/// ACME directory URL (e.g. Let's Encrypt production/staging, or custom CA)
#[arg(long)]
acme_url: Option<String>,

/// Domain to provision ACME certificate for
#[arg(long)]
acme_domain: Option<String>,
```

Both are required together — error at startup if only one is set. Mutually
exclusive with `--dev`.

#### Crate: `instant-acme` (0.8.5)

Lower-level ACME client that gives full control over the HTTP-01 challenge
flow. Chosen over `rustls-acme`/`tokio-rustls-acme` because:

1. We need to share the cert between HTTPS (axum) and QUIC (moq-native) — the
   higher-level crates wrap a single TCP acceptor.
2. We need HTTP-01 specifically (validated via our existing axum HTTP server),
   not TLS-ALPN-01.
3. We need a custom ACME directory URL (not just Let's Encrypt).

#### Startup flow (when `--acme-url` + `--acme-domain` are set)

```
1. Start HTTP server on --http-bind
   └─ serves content + /.well-known/acme-challenge/{token} route
   └─ challenge tokens stored in Arc<RwLock<HashMap<String, String>>>

2. Check for cached cert in data_dir/certs/{domain}.{crt,key}
   └─ if valid and >30 days from expiry → skip to step 5

3. ACME provisioning (blocking before QUIC/HTTPS start):
   a. Load or create account key from data_dir/acme_account.key
   b. Create ACME account (or use existing) at --acme-url
   c. Create order for --acme-domain
   d. Get HTTP-01 challenge → store token+response in shared map
   e. Tell ACME server to validate
   f. Generate CSR via rcgen, finalize order
   g. Download cert chain
   h. Write cert+key to data_dir/certs/{domain}.{crt,key}
   i. Clear challenge from shared map

4. Write cert + key PEM files to data_dir/certs/

5. Init QUIC server with cert paths:
   server_config.tls.cert = vec![certs_dir.join("{domain}.crt")]
   server_config.tls.key = vec![certs_dir.join("{domain}.key")]

6. Start HTTPS server on --bind addr (TCP) with same cert
   └─ uses axum-server with RustlsConfig::from_pem_file()

7. Enable HTTP→HTTPS redirect middleware:
   └─ all requests except /.well-known/acme-challenge/* get 301 to https://

8. Spawn renewal task
9. Enter QUIC accept loop
```

#### ACME challenge serving

```rust
// Shared state between ACME provisioner and HTTP server
let acme_challenges: Arc<RwLock<HashMap<String, String>>> = Default::default();

// Added to the HTTP router:
.route(
    "/.well-known/acme-challenge/{token}",
    get(|Path(token): Path<String>, State(state): State<_>| async move {
        let challenges = state.acme_challenges.read().unwrap();
        match challenges.get(&token) {
            Some(response) => (StatusCode::OK, response.clone()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }),
)
```

#### HTTP → HTTPS redirect

After certs are provisioned, a middleware layer on the HTTP router redirects
all non-ACME requests:

```rust
async fn redirect_to_https(
    Host(host): Host,
    uri: Uri,
    next: Next,
) -> Response {
    // Let ACME challenges through
    if uri.path().starts_with("/.well-known/acme-challenge/") {
        return next.run(req).await;
    }
    // 301 redirect to HTTPS
    let https_uri = format!("https://{host}{uri}");
    Redirect::permanent(&https_uri).into_response()
}
```

#### Cert renewal

A background tokio task handles automatic renewal:

```rust
// Spawned after initial provisioning
tokio::spawn(async move {
    loop {
        tokio::time::sleep(Duration::from_secs(12 * 3600)).await; // check every 12h

        let cert = load_cert_from_disk(&certs_dir, &domain)?;
        let days_remaining = cert.not_after() - now();

        if days_remaining > Duration::from_secs(30 * 86400) {
            continue; // >30 days remaining, skip
        }

        tracing::info!(days_remaining = days_remaining.as_secs() / 86400, "renewing cert");

        // Re-run ACME flow (account key reused, challenges served on HTTP)
        provision_acme(&acme_url, &domain, &account_key, &certs_dir, &challenges).await?;

        // Reload HTTPS server certs
        rustls_config.reload_from_pem_file(&cert_path, &key_path).await?;

        // Reload QUIC server certs (moq-native's existing reload mechanism)
        // Option A: send SIGUSR1 to self
        // Option B: call server.reload_certs() if exposed
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGUSR1)?;

        tracing::info!("cert renewed");
    }
});
```

#### ACME account key persistence

The ACME account private key is stored at `data_dir/acme_account.key` (PEM
format). On first run it's generated and registered with the CA. On subsequent
runs the existing key is loaded, avoiding re-registration. The `instant-acme`
crate handles both paths via `Account::create()` with an `ExternalAccountBinding`
or re-loading from serialized credentials.

#### Cert storage layout

```
data_dir/
  iroh_secret_key          # iroh endpoint identity (existing)
  acme_account.key         # ACME account private key (new)
  certs/
    {domain}.crt           # full cert chain PEM
    {domain}.key           # private key PEM
```

#### Dependencies to add

```toml
instant-acme = "0.8"      # ACME client
axum-server = { version = "0.7", features = ["tls-rustls"] }  # HTTPS serving with rustls reload
rcgen = "0.13"             # CSR generation for ACME finalization
```

(`rcgen` may already be a transitive dep via moq-native's self-signed cert
generation, but we need it directly for CSR creation.)

#### Error handling

- If ACME provisioning fails at startup, the relay exits with a clear error
  (no fallback to self-signed — that's what `--dev` is for).
- If renewal fails, log `error!` and retry at next 12h interval. The existing
  cert remains valid (renewal starts 30 days before expiry, giving many retry
  windows).
- Rate limit awareness: Let's Encrypt has rate limits (5 certs/week/domain).
  Log the ACME directory URL at startup so operators can verify they're hitting
  staging vs production.

### Step 8: Tickets

**8a) Shared ticket trait**: define a `Ticket` trait in `iroh-live` that all
ticket types implement. The trait provides serialization to/from a compact
string format (for CLI args and QR codes), and accessors for the common fields.

```rust
pub trait Ticket: Display + FromStr {
    /// The peer's endpoint address.
    fn endpoint_addr(&self) -> &EndpointAddr;
    /// The broadcast or call name.
    fn name(&self) -> &str;
    /// Relay URLs the ticket was created with (may be empty for P2P-only).
    fn relay_urls(&self) -> &[Url];
}
```

**8b) Verify `LiveTicket` and `CallTicket`**: both types must support
`EndpointAddr`, `name`, and a list of relay URLs. The relay URLs enable clients
to reach the publisher through the relay when direct P2P is not available. Check
the existing `iroh-live/src/ticket.rs` and extend if needed — in particular,
the `relay_urls` field may not exist yet.

**8c) Display**: the relay's web page shows:
- **Browser URL**: `https://relay.example.com/?name=hello` (copyable, QR-codeable)
- **CLI ticket**: `iroh:<endpoint_id>?name=hello&relay=<url>` (for `--relay` flag)

QR code: use a small JS library (e.g. `qrcode-generator`, ~4KB) or the relay
generates a QR SVG server-side and injects it into the page.

The relay binary prints both on startup:
```
Browser: https://example.com/?name=hello
CLI:     iroh-live-relay://<endpoint_id>
```

## Implementation order

1. **iroh-moq**: add `publish_broadcast()` to `MoqSession`
2. **Test pattern**: add blinking yellow marker to `make_test_pattern()`
3. **iroh-live-relay crate**: new workspace member, `RelayServer` struct with data dir persistence
4. **Web app**: Vite project in `iroh-live-relay/web/`, build → `web/dist/`
5. **Static serving**: fork `Web::run()`, combine relay routes + static files
6. **Publish example**: add `--test-source` and `--relay` flags (relay is additive)
7. **Manual smoke test**: publish from CLI → watch in browser
8. **Playwright setup**: package.json, config, fixtures with tempdir + port 0
9. **E2E: iroh-to-browser**: CLI publish → relay → browser watch, with video content verification
10. **E2E: browser-to-iroh**: browser publish → relay → CLI subscribe
11. **Tickets**: shared trait, relay URL support in LiveTicket/CallTicket
12. **ACME**: `--acme-domain` flag, cert provisioning + reload
13. **QR + tickets**: display in web UI and CLI output

## Files to create/modify

| File | Action |
|------|--------|
| `Cargo.toml` (root) | Add `iroh-live-relay` to workspace members |
| `iroh-moq/src/lib.rs` | Add `publish_broadcast()`, `origin_producer()`, `origin_consumer()` to MoqSession |
| `rusty-codecs/src/codec/test_util.rs` | Add blinking yellow marker to `make_test_pattern()` |
| `iroh-live-relay/Cargo.toml` | New binary crate (includes `dirs` dep) |
| `iroh-live-relay/src/main.rs` | `RelayServer` struct + relay binary (~200 lines) |
| `iroh-live-relay/web/package.json` | Vite + @moq/* deps |
| `iroh-live-relay/web/vite.config.ts` | Build config |
| `iroh-live-relay/web/src/index.html` | Watch page |
| `iroh-live-relay/web/src/publish.html` | Publish page |
| `iroh-live-relay/web/src/index.ts` | Watch entry |
| `iroh-live-relay/web/src/publish.ts` | Publish entry |
| `iroh-live/examples/publish.rs` | Add `--test-source`, `--relay` flags (additive) |
| `iroh-live/Cargo.toml` | Add `test-util` as optional dep for examples |
| `iroh-live/src/ticket.rs` | Shared `Ticket` trait, relay URL support |
| `tests/e2e-browser/package.json` | Playwright |
| `tests/e2e-browser/playwright.config.ts` | Chrome config with fake media flags |
| `tests/e2e-browser/tests/iroh-to-browser.spec.ts` | CLI→browser test with video verification |
| `tests/e2e-browser/tests/browser-to-iroh.spec.ts` | Browser→CLI test |
| `tests/e2e-browser/fixtures/relay.ts` | Relay process fixture (tempdir, port 0) |

## Risks and mitigations

- **moq-relay Web not extensible**: `Web::run()` builds router internally. Mitigation:
  fork the ~30 lines of router setup in our main.rs, injecting static routes.
- **@moq/* npm packages ship raw .tsx**: Need solid-js + vite-plugin-solid at build
  time. Mitigation: match the devDependencies from `../moq/js/demo/package.json`.
- **Self-signed certs + WebTransport**: Chrome needs `--ignore-certificate-errors` or
  `--origin-to-force-quic-on`. The `<moq-watch>` already handles fingerprint fetch
  from `/certificate.sha256` for `http://` URLs (insecure dev path). For Playwright,
  launch args handle this.
- **Headless camera/mic**: Chrome `--use-fake-device-for-media-stream` provides a
  test pattern and tone. `<moq-publish source="camera">` picks these up.
- **ACME + QUIC cert sharing**: Both noq's `ServeCerts` and axum's `RustlsConfig`
  support cert reload from disk. ACME writes to disk, then triggers reload.
- **`OriginProducer::insert` path semantics**: moq-lite uses paths. Verify that a
  flat name like `"hello"` works without prefix. The relay's `Connection` extracts
  path from the URL; for iroh connections there's no URL, so the path comes from
  the moq-lite `announced()` stream.
- **Video verification flakiness**: codec compression can shift colors. Use generous
  tolerance for yellow detection (~50 per channel). The blinking pattern (15 frames
  on/off) is slow enough that 40 screenshots at 50ms intervals will capture multiple
  transitions.

## Verification

1. `cd iroh-live-relay/web && bun install && bun run build`
2. `cargo build -p iroh-live-relay`
3. `cargo run -p iroh-live-relay -- --dev` → relay starts, prints iroh endpoint ID and HTTP port
4. Open `http://localhost:<port>/` in Chrome → watch page loads
5. `cargo run --example publish -- --test-source --relay <iroh-addr>` → publishes P2P + relay
6. Browser shows video from CLI publisher (blinking yellow square visible)
7. Open `http://localhost:<port>/publish.html?name=test` → publishes from browser
8. `cd tests/e2e-browser && npm install && npm run test` → Playwright tests pass
