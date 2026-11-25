//! The `qlog` feature: a connection writes its trace into `QLOGDIR`.
//!
//! This binary holds one test, because it changes `QLOGDIR` for the whole
//! process. The test changes it only before the runtime starts, while no
//! other thread can read the environment.

use std::path::Path;

use iroh::{
    Endpoint, RelayMode,
    endpoint::{Builder, Connection},
};
use iroh_live_node::{EndpointConfig, Mdns, MoqPreset};

/// The ALPN the test endpoints speak.
const ALPN: &[u8] = b"iroh-live/qlog-test";

/// An endpoint traces its connections if `QLOGDIR` was set when it was built.
///
/// Two endpoints built with [`MoqPreset`] while `QLOGDIR` is unset connect
/// after it is set, and write nothing. Two endpoints built with
/// [`EndpointConfig`] after it is set write a client and a server trace that
/// share the connection id.
#[test]
fn endpoints_trace_connections_if_qlogdir_was_set_when_built() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("qlog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // SAFETY: no other thread runs yet: the runtime starts below, and this
    // binary holds no other test.
    unsafe { std::env::remove_var("QLOGDIR") };
    // The preset builds the transport config, and so reads `QLOGDIR`, here.
    let untraced = [Endpoint::builder(MoqPreset), Endpoint::builder(MoqPreset)];
    // SAFETY: as above, no other thread runs yet.
    unsafe { std::env::set_var("QLOGDIR", &dir) };

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build the runtime")
        .block_on(async {
            let [server, client] = untraced;
            connect_and_close(bind(server).await, bind(client).await).await;
            assert!(
                !dir.exists(),
                "endpoints built without QLOGDIR created {}",
                dir.display()
            );

            let server = bind(configured().await).await;
            let client = bind(configured().await).await;
            connect_and_close(server, client).await;
        });

    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("QLOGDIR was not created")
        .map(|entry| entry.expect("failed to read QLOGDIR").path())
        .inspect(|path| {
            let len = std::fs::metadata(path).expect("no metadata").len();
            assert!(len > 0, "{} is empty", path.display());
        })
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    let _ = std::fs::remove_dir_all(&dir);

    let [first, second] = names.as_slice() else {
        panic!("expected one client and one server trace, found {names:?}");
    };
    let client = connection_id(first, "client").or_else(|| connection_id(second, "client"));
    let server = connection_id(first, "server").or_else(|| connection_id(second, "server"));
    let (Some(client), Some(server)) = (client, server) else {
        panic!("expected one client and one server trace, found {names:?}");
    };
    assert_eq!(client, server, "the two ends name different connection ids");
}

/// Returns the connection id in `name`, if it is an `iroh-live-` trace of `side`.
fn connection_id<'a>(name: &'a str, side: &str) -> Option<&'a str> {
    let stem = name
        .strip_prefix("iroh-live-")?
        .strip_suffix(&format!("-{side}.qlog"))?;
    let (_unix_ms, id) = stem.split_once('-')?;
    Some(id)
}

/// Returns a builder set up by [`EndpointConfig`] without mDNS.
async fn configured() -> Builder {
    EndpointConfig::default()
        .with_mdns(Mdns::Off)
        .apply(Builder::empty())
        .await
}

/// Binds `builder` for local use only: no relay and no address lookup.
async fn bind(builder: Builder) -> Endpoint {
    builder
        .relay_mode(RelayMode::Disabled)
        .clear_address_lookup()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .expect("failed to bind endpoint")
}

/// Opens a connection from `client` to `server`, then closes both endpoints.
async fn connect_and_close(server: Endpoint, client: Endpoint) {
    let accept = async {
        let incoming = server.accept().await.expect("the server endpoint closed");
        incoming.await.expect("failed to accept")
    };
    let (accepted, dialed): (Connection, _) =
        tokio::join!(accept, client.connect(server.addr(), ALPN));
    let dialed = dialed.expect("failed to dial");
    drop((accepted, dialed));

    // Closing the endpoints ends the connections, which flushes their traces.
    tokio::join!(client.close(), server.close());
}
