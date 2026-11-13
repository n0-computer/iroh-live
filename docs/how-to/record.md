# Record a broadcast

This page writes a broadcast to a file without decoding it: the encoded
frames go straight into the container.

## With `irl`

```sh
irl record <TICKET> -o clip.mp4 --duration 10
irl record <TICKET> -o talk.mkv --rendition 720p
```

The extension picks the container: `.mp4`, `.m4v` and `.m4s` are fragmented
MP4, and `.mkv` and `.webm` are Matroska. Without `--rendition`, the file has
one video track per rendition. Both containers are fragmented, so a recording
that was interrupted plays up to the last fragment written. The
[CLI reference](../reference/cli.md#irl-record) lists every flag.

To record several broadcasts from one endpoint, use `irl run` with a `record`
key, see [The `irl run` session file](../reference/irl-run.md).

## From Rust

`RemoteBroadcast::mux_source()` hands a subscribed broadcast to moq-mux's
exporters. It needs the `record` feature, which `full` includes, and a direct
dependency on `moq-mux` at the same rev as iroh-live's (see
[Install](../getting-started/install.md#use-the-library)).

```rust,no_run
use iroh_live::{
    BroadcastTicket, EndpointConfig, Node, NodeConfig, media::remote_broadcast,
};
use moq_mux::{catalog::Stream as _, container::fmp4};
use tokio::io::AsyncWriteExt as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ticket: BroadcastTicket = std::env::args()
        .nth(1)
        .ok_or("usage: record <ticket>")?
        .parse()?;
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(endpoint.clone(), NodeConfig::default());
    let subscription = node.subscribe_ticket(&ticket).await?;
    let broadcast = remote_broadcast(&subscription);

    // Waits for the catalog.
    let source = broadcast.mux_source().await?;
    let catalog = moq_mux::catalog::Consumer::<()>::new(&source.broadcast(), Default::default())
        .await?
        .select(moq_mux::select::Broadcast::default());
    let mut export = fmp4::Export::new(source, catalog);

    let mut file = tokio::fs::File::create("recording.mp4").await?;
    tokio::select! {
        result = async {
            while let Some(chunk) = export.next().await? {
                file.write_all(&chunk).await?;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        } => result?,
        _ = tokio::signal::ctrl_c() => {}
    }
    file.flush().await?;

    node.shutdown().await;
    endpoint.close().await;
    Ok(())
}
```

`moq_mux::select::Broadcast` picks the tracks: by default every rendition.
`select::Video::default().name("720p")` keeps one. `moq_mux::container::mkv`
writes Matroska instead. `iroh-live-cli/src/record.rs` adds a limit on how
late a group may be before the export skips it.

## Where to go next

- [Watch a broadcast and render it](watch.md)
- The [moq-mux docs](https://doc.moq.dev/lib/rs/crate/moq-mux) describe the
  containers.
