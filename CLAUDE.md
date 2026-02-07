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

## Commits

- Small incremental commits, each leaving all crates compiling
- `cargo clippy --workspace` must be clean (no warnings)
- `cargo fmt --check` must pass
- Concise commit messages
