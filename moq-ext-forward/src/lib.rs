//! Serves a broadcast created in one origin from a broadcast held elsewhere.
//!
//! moq-net creates every broadcast inside the origin that offers it, and only
//! its own sessions can hand an origin a broadcast they already hold. An
//! application that offers one broadcast in several origins (say, one per
//! peer, each seeing a different set), or re-serves what one origin learned in
//! another, has to copy: [`forward`] subscribes each track a reader asks for in
//! `dest` in `source`, and writes its groups and frames through. The readers'
//! demand (delay, priority, ranges) travels upstream as it changes. Frames share their payload, so a copy moves references, not
//! bytes. Nothing is read from `source` that no reader of `dest` asked for.

use moq_net::{broadcast, group, track};
use n0_future::task::JoinSet;
use tracing::{debug, trace};

/// Serves `dest`'s track requests from `source` until either ends.
///
/// Closes `dest` once `source` closes, so the copy never outlives what it
/// copies.
pub fn forward(source: broadcast::Consumer, dest: broadcast::Producer) -> impl Future<Output = ()> {
    forward_with(source, dest, || {})
}

/// Like [`forward`], calling `requested` whenever a reader asks for a track.
///
/// The request handler is registered when this is called, not when the future
/// is first polled: a reader may ask for a track in between, and with no
/// handler yet the broadcast would answer `NotFound`.
pub fn forward_with(
    source: broadcast::Consumer,
    dest: broadcast::Producer,
    requested: impl Fn(),
) -> impl Future<Output = ()> {
    let mut requests = dest.requests();
    async move {
        let mut tracks = JoinSet::new();
        loop {
            tokio::select! {
                _ = source.closed() => break,
                request = moq_net::kio::wait(|waiter| requests.poll_requested_track(waiter)) => {
                    let Ok(request) = request else { break };
                    requested();
                    tracks.spawn(track(source.clone(), request));
                }
                Some(_) = tracks.join_next(), if !tracks.is_empty() => {}
            }
        }
        dest.close();
    }
}

/// Copies one requested track from `source`.
async fn track(source: broadcast::Consumer, request: track::Request) {
    let name = request.name().to_owned();
    let upstream = match source.track(&name) {
        Ok(upstream) => upstream,
        Err(err) => {
            debug!(track = %name, %err, "source has no such track");
            request.reject(err);
            return;
        }
    };
    let info = match upstream.info().await {
        Ok(info) => info,
        Err(err) => {
            debug!(track = %name, %err, "source track did not resolve");
            request.reject(err);
            return;
        }
    };
    let subscription = request.subscription();
    let mut subscriber = match upstream.subscribe(subscription).await {
        Ok(subscriber) => subscriber,
        Err(err) => {
            request.reject(err);
            return;
        }
    };
    let mut producer = request.accept(info);
    let mut groups = JoinSet::new();
    loop {
        tokio::select! {
            // The readers' demand (delay, priority, ranges) travels upstream,
            // as a relay's SUBSCRIBE_UPDATE would.
            changed = producer.subscription_changed() => match changed {
                Ok(Some(subscription)) => {
                    if let Err(err) = subscriber.update(subscription) {
                        trace!(track = %name, %err, "subscription not updated");
                    }
                }
                // Nobody reads the copy any more.
                Ok(None) | Err(_) => break,
            },
            next = subscriber.recv_group() => match next {
                Ok(Some(upstream)) => {
                    let dest = match producer.create_group(group::Info { sequence: upstream.sequence }) {
                        Ok(dest) => dest,
                        // A group the reader already moved past.
                        Err(err) => {
                            trace!(track = %name, %err, "group not copied");
                            continue;
                        }
                    };
                    groups.spawn(copy_group(upstream, dest));
                }
                Ok(None) => {
                    let _ = producer.finish();
                    break;
                }
                Err(err) => {
                    let _ = producer.abort(err);
                    break;
                }
            },
            Some(_) = groups.join_next(), if !groups.is_empty() => {}
        }
    }
    while groups.join_next().await.is_some() {}
}

/// Copies one group's frames.
async fn copy_group(mut source: group::Consumer, mut dest: group::Producer) {
    loop {
        match source.read_frame().await {
            Ok(Some(frame)) => {
                if dest.write_frame(frame.timestamp, frame.payload).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = dest.finish();
                return;
            }
            Err(err) => {
                let _ = dest.abort(err);
                return;
            }
        }
    }
}
