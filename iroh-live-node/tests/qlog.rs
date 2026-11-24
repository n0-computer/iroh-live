//! The `qlog` feature: a connection writes its trace into `QLOGDIR`.
//!
//! This binary holds one test, because it sets `QLOGDIR` for the whole process.

use std::path::Path;

use iroh::{Endpoint, endpoint::presets};
use iroh_live_node::Congestion;

/// The ALPN the two test endpoints speak.
const ALPN: &[u8] = b"iroh-live/qlog-test";

/// A connection between two local endpoints writes a client and a server trace.
#[test]
fn a_connection_writes_qlog_files_into_qlogdir() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("qlog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: no other thread runs yet: the runtime starts below, and this
    // binary holds no other test.
    unsafe { std::env::set_var("QLOGDIR", &dir) };

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build the runtime")
        .block_on(connect_and_close());

    let names: Vec<String> = std::fs::read_dir(&dir)
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
    for side in ["client", "server"] {
        assert!(
            names
                .iter()
                .any(|name| name.starts_with("iroh-live-")
                    && name.ends_with(&format!("-{side}.qlog"))),
            "no {side} trace among {names:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Opens a connection between two endpoints, then closes both endpoints.
async fn connect_and_close() {
    let server = bind().await;
    let client = bind().await;

    let accept = async {
        let incoming = server.accept().await.expect("the server endpoint closed");
        incoming.await.expect("failed to accept")
    };
    let (accepted, dialed) = tokio::join!(accept, client.connect(server.addr(), ALPN));
    let dialed = dialed.expect("failed to dial");
    drop((accepted, dialed));

    // Closing the endpoints ends the connections, which flushes their traces.
    tokio::join!(client.close(), server.close());
}

/// Binds a local endpoint with BBR3's transport config, which reads `QLOGDIR`.
async fn bind() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .transport_config(Congestion::Bbr3.transport_config())
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .expect("failed to bind endpoint")
}
