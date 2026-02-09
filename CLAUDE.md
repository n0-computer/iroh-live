# iroh-live

Rust workspace for real-time media over iroh (QUIC-based transport).

## Crates

| Crate | Description |
|-------|-------------|
| `iroh-live` | High-level API for live audio/video sessions and rooms |
| `iroh-moq` | Media-over-QUIC transport protocol |
| `moq-media` | Media capture, encoding, decoding, and processing |
| `web-transport-iroh` | WebTransport implementation over iroh/quinn |

## Build

```sh
cargo build --workspace
cargo build --workspace --all-features
```

## Test

```sh
cargo test --workspace
cargo test --workspace --all-features
```

## Lint

```sh
cargo clippy --workspace --all-features
cargo fmt --check
```

## Shell

- Always use `rg` (ripgrep) instead of `grep` in shell commands

## Commits

- Small incremental commits, each leaving all crates compiling
- `cargo clippy --workspace` must be clean (no warnings)
- `cargo fmt --check` must pass
- Concise commit messages

## iroh Connection & Path Stats

Access chain: `MoqSession::conn()` → `&Connection<HandshakeCompleted>` (from `iroh::endpoint`).

### Connection-level

- `conn.stats()` → `ConnectionStats { udp_tx: UdpStats, udp_rx: UdpStats, frame_tx: FrameStats, frame_rx: FrameStats }`
- `conn.remote_id()` → `EndpointId`
- `conn.close_reason()` → `Option<SessionError>` (via web-transport-iroh Session wrapper)
- `conn.paths()` → `impl Watcher<Value = PathInfoList>` — call `.get()` for current snapshot

### Per-path stats

`conn.paths().get()` → `PathInfoList`, iterate with `.iter()` → `PathInfo`:

- `path_info.remote_addr()` → `TransportAddr` (`.is_ip()` / `.is_relay()`)
- `path_info.is_selected()` → `bool` (true = currently used for transmission)
- `path_info.rtt()` → `Duration`
- `path_info.stats()` → `PathStats` (from `iroh_quinn_proto`):
  - `rtt: Duration`
  - `udp_tx: UdpStats { datagrams: u64, bytes: u64, ios: u64 }`
  - `udp_rx: UdpStats { datagrams: u64, bytes: u64, ios: u64 }`
  - `cwnd: u64` — congestion window
  - `congestion_events: u64`
  - `lost_packets: u64`
  - `lost_bytes: u64`
  - `sent_plpmtud_probes: u64`
  - `lost_plpmtud_probes: u64`
  - `black_holes_detected: u64`
  - `current_mtu: u16`

### Loss rate calculation

`loss_rate = lost_packets / (udp_tx.datagrams + lost_packets)` (from selected path)

### StatsSmoother

`iroh_live::util::StatsSmoother` — smooths bandwidth/RTT over 1s intervals. Call `smoothed(|| conn.stats())` → `SmoothedStats { rtt, up: &Rate, down: &Rate }` where `Rate { total: u64, rate: f32, rate_str: String }`.
