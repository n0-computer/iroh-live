//! Serves a broadcast created in one origin from a broadcast held elsewhere.
//!
//! moq-net creates every broadcast inside the origin that offers it, and only
//! its own sessions can hand an origin a broadcast they already hold. An
//! application that offers one broadcast in several origins (say, one per
//! peer, each seeing a different set), or re-serves what one origin learned in
//! another, has to copy. [`forward`] subscribes in `source` to each track a
//! reader asks for in `dest`, and writes its groups and frames through. The
//! readers' demand (delay, priority, ranges) travels upstream as it changes.
//! Frames share their payload, so a copy moves references, not bytes. Nothing
//! is read from `source` that no reader of `dest` asked for.

use std::{task::Poll, time::Duration};

use moq_net::{broadcast, group, track};
use n0_future::task::JoinSet;
use tracing::{debug, trace};

/// How long copies may keep running after the source closed.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Serves `dest`'s track requests from `source` until either ends.
///
/// Closes `dest` once `source` closes, so the copy never outlives what it
/// copies. Tracks being copied then get a few seconds to reach their ends, so
/// readers see the tail and the end, not a dropped producer.
///
/// Dropping the future stops the copy at once.
pub fn forward(source: broadcast::Consumer, dest: broadcast::Producer) -> impl Future<Output = ()> {
    forward_with(source, dest, || {})
}

/// Serves requests as [`forward`] does, and calls `requested` for each one.
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
                _ = source.closed() => {
                    // Stop taking requests, and let the running copies finish.
                    let drain = async { while tracks.join_next().await.is_some() {} };
                    if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
                        debug!("tracks did not drain after the source closed");
                    }
                    break;
                }
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

/// What a track's loop woke for.
#[allow(clippy::large_enum_variant, reason = "a short-lived local")]
enum Wake {
    /// The readers' demand changed, or the last reader left.
    Demand(Result<Option<track::Subscription>, moq_net::Error>),
    /// The source opened a group, or ended.
    Group(Result<Option<group::Consumer>, moq_net::Error>),
    /// A group's copy finished.
    Copied,
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
            debug!(track = %name, %err, "source track did not accept the subscription");
            request.reject(err);
            return;
        }
    };
    let mut producer = request.accept(info);
    // The requester may have left in the meantime, and then no change is ever
    // reported: nobody reads the copy, so do not hold the upstream for it.
    if producer.subscription().is_none() {
        match producer.abort_unwanted(moq_net::Error::Cancel) {
            Ok(()) => return,
            Err(back) => producer = back,
        }
    }
    let mut groups = JoinSet::new();
    loop {
        let wake = tokio::select! {
            changed = producer.subscription_changed() => Wake::Demand(changed),
            next = subscriber.recv_group() => Wake::Group(next),
            Some(_) = groups.join_next(), if !groups.is_empty() => Wake::Copied,
        };
        match wake {
            // The readers' demand (delay, priority, ranges) travels upstream,
            // as a relay's SUBSCRIBE_UPDATE would.
            Wake::Demand(Ok(Some(subscription))) => {
                if let Err(err) = subscriber.update(subscription) {
                    trace!(track = %name, %err, "subscription not updated");
                }
            }
            // Nobody reads the copy any more. End it only if no reader joined
            // meanwhile: a plain drop would hand such a reader `Dropped`.
            Wake::Demand(Ok(None)) => match producer.abort_unwanted(moq_net::Error::Cancel) {
                Ok(()) => break,
                Err(back) => producer = back,
            },
            Wake::Demand(Err(_)) => break,
            Wake::Group(Ok(Some(upstream))) => {
                let dest = match producer.create_group(group::Info {
                    sequence: upstream.sequence,
                }) {
                    Ok(dest) => dest,
                    // A group the reader already moved past.
                    Err(err) => {
                        trace!(track = %name, %err, "group not copied");
                        continue;
                    }
                };
                groups.spawn(copy_group(upstream, dest));
            }
            Wake::Group(Ok(None)) => {
                // Carry the source's declared end over when it has one.
                let declared = match subscriber.poll_finished(&moq_net::kio::Waiter::noop()) {
                    Poll::Ready(Ok(end)) => producer.finish_at(end, None).is_ok(),
                    _ => false,
                };
                if !declared {
                    let _ = producer.finish();
                }
                break;
            }
            Wake::Group(Err(err)) => {
                let _ = producer.abort(err);
                break;
            }
            Wake::Copied => {}
        }
    }
    // Dropped first, so no new group starts while the open ones drain.
    drop(subscriber);
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

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    fn demand() -> track::Subscription {
        track::Subscription::default()
    }

    /// A requester that leaves before the request is served leaves no reader,
    /// so the upstream track is not held for it.
    #[tokio::test]
    async fn a_requester_that_left_does_not_hold_the_upstream() {
        let source = broadcast::Info::new().produce();
        let upstream = source
            .create_track("video", track::Info::default())
            .unwrap();
        let dest = broadcast::Info::new().produce();
        let copy = tokio::spawn(forward(source.consume(), dest.clone()));

        // Subscribed and dropped without yielding, so the forward has not
        // looked at the request yet.
        let pending = dest.consume().track("video").unwrap().subscribe(demand());
        drop(pending);

        tokio::time::timeout(WAIT, async {
            while upstream.subscription().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the upstream is still subscribed");
        copy.abort();
    }

    /// When the source ends cleanly and closes, a reader gets every group and
    /// then the end, not `Dropped`.
    #[tokio::test]
    async fn a_clean_end_reaches_the_reader_as_an_end() {
        let source = broadcast::Info::new().produce();
        let mut upstream = source
            .create_track("video", track::Info::default())
            .unwrap();
        let dest = broadcast::Info::new().produce();
        let copy = tokio::spawn(forward(source.consume(), dest.clone()));

        let mut reader = dest
            .consume()
            .track("video")
            .unwrap()
            .subscribe(demand())
            .await
            .unwrap();
        for n in 0..3u8 {
            upstream.write_frame(None, Bytes::from(vec![n])).unwrap();
        }
        upstream.finish().unwrap();
        drop(upstream);
        drop(source);

        tokio::time::timeout(WAIT, async {
            let mut last = None;
            while let Some(mut group) = reader.recv_group().await.expect("ended with an error") {
                while let Some(frame) = group.read_frame().await.expect("frame error") {
                    last = Some(frame.payload);
                }
            }
            assert_eq!(last.as_deref(), Some([2u8].as_slice()), "the tail was lost");
        })
        .await
        .expect("the reader never saw the end");
        tokio::time::timeout(WAIT, copy).await.unwrap().unwrap();
    }
}
