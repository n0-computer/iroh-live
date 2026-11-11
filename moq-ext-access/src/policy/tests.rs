use std::sync::Mutex;

use moq_net::Pattern;
use n0_future::{StreamExt, future::block_on, stream};

use super::*;

type Peer = String;

/// Returns a connect request of `peer` for the root, with no token.
fn request(peer: &str) -> PeerRequest<Peer> {
    PeerRequest::new(
        peer.to_owned(),
        SessionRequest::default(),
        false,
        Event::Connect,
    )
}

/// Returns a grant that lets a peer publish under `prefix/**`.
fn under(prefix: &str) -> Grant {
    let pattern: Pattern = format!("{prefix}/**").parse().expect("a pattern");
    Grant::publish_under(pattern)
}

fn decide(policy: &impl Policy<Peer>, request: &PeerRequest<Peer>) -> Decision {
    block_on(policy.decide(request))
}

/// A policy whose change stream yields once per item a test sends.
#[derive(Debug)]
struct Changing(Mutex<Option<async_channel::Receiver<()>>>);

impl Changing {
    fn new() -> (Self, async_channel::Sender<()>) {
        let (sender, receiver) = async_channel::bounded(4);
        (Self(Mutex::new(Some(receiver))), sender)
    }
}

impl Policy<Peer> for Changing {
    async fn decide(&self, _request: &PeerRequest<Peer>) -> Decision {
        Err(Rejection::new("changing"))
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        let receiver = self.0.lock().expect("poisoned").take()?;
        Some(Box::pin(receiver))
    }
}

#[test]
fn a_request_reads_through_to_the_session_request() {
    let session = SessionRequest::new("/studio?jwt=abc&x=1");
    let request = PeerRequest::new("bob".to_owned(), session, true, Event::Revalidate);
    assert_eq!(request.peer(), "bob");
    assert!(request.dialed());
    assert_eq!(request.event(), Event::Revalidate);
    assert_eq!(request.path(), "studio");
    assert_eq!(request.query("x"), Some("1"));
    assert_eq!(request.token(), Some("abc"));
}

#[test]
fn open_admits_with_its_grant() {
    let decision = decide(&Open(under("a")), &request("bob"));
    assert_eq!(decision.expect("admitted"), Authorization::from(under("a")));
}

#[test]
fn closed_rejects_as_unauthorized() {
    let rejection = decide(&Closed, &request("bob")).expect_err("rejected");
    assert!(
        matches!(rejection.code(), moq_net::Error::Unauthorized),
        "{rejection}"
    );
}

#[test]
fn a_rejection_carries_its_code_and_reason() {
    let rejection = Rejection::new("banned").with_code(moq_net::Error::App(7));
    assert!(matches!(rejection.code(), moq_net::Error::App(7)));
    assert_eq!(rejection.reason(), "banned");
    assert!(rejection.to_string().contains("banned"), "{rejection}");
}

#[test]
fn from_fn_sees_the_request() {
    let policy = from_fn(|request: &PeerRequest<Peer>| {
        if request.peer() == "bob" && request.token() == Some("t") {
            Ok(Grant::everything().into())
        } else {
            Err(Rejection::new("who are you"))
        }
    });
    let with_token = PeerRequest::new(
        "bob".to_owned(),
        SessionRequest::new("/?jwt=t"),
        false,
        Event::Connect,
    );
    assert!(decide(&policy, &with_token).is_ok());
    assert!(decide(&policy, &request("bob")).is_err());
}

#[test]
fn a_union_of_an_admit_and_a_reject_admits_with_the_admitting_grant() {
    let decision = decide(
        &Policy::<Peer>::union(Open(under("a")), Closed),
        &request("bob"),
    );
    assert_eq!(decision.expect("admitted").grant, under("a"));
    let decision = decide(
        &Policy::<Peer>::union(Closed, Open(under("b"))),
        &request("bob"),
    );
    assert_eq!(decision.expect("admitted").grant, under("b"));
}

#[test]
fn a_union_of_two_rejects_keeps_the_first_rejection() {
    let first = from_fn(|_: &PeerRequest<Peer>| Err(Rejection::new("first")));
    let rejection = decide(&first.union(Closed), &request("bob")).expect_err("rejected");
    assert_eq!(rejection.reason(), "first");
}

#[test]
fn a_union_of_two_admits_joins_patterns_and_takes_the_earliest_expiry() {
    let soon = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let later = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let a = from_fn(move |_: &PeerRequest<Peer>| {
        Ok(Authorization {
            grant: under("a"),
            transit: Transit {
                forward_to: Pattern::all().into(),
                ..Default::default()
            },
            expires: Some(later),
            revalidate: None,
        })
    });
    let b = from_fn(move |_: &PeerRequest<Peer>| {
        Ok(Authorization {
            grant: under("b"),
            transit: Transit {
                peer_forwards: true,
                ..Default::default()
            },
            expires: Some(soon),
            revalidate: Some(Duration::from_secs(30)),
        })
    });
    let joined = decide(&a.union(b), &request("bob")).expect("admitted");
    assert!(joined.grant.allows_publish("a/cam"));
    assert!(joined.grant.allows_publish("b/cam"));
    assert!(!joined.grant.allows_publish("c/cam"));
    assert!(joined.transit.forward_to.matches("any/path"));
    assert!(joined.transit.peer_forwards);
    assert_eq!(joined.expires, Some(soon));
    assert_eq!(joined.revalidate, Some(Duration::from_secs(30)));
}

#[test]
fn a_filter_rejects_what_its_predicate_refuses() {
    let policy = Open(Grant::everything()).filter(|request| request.peer() != "mallory");
    assert!(decide(&policy, &request("bob")).is_ok());
    let rejection = decide(&policy, &request("mallory")).expect_err("filtered");
    assert_eq!(rejection.reason(), "filtered");
    assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
}

#[test]
fn a_filter_keeps_the_inner_changes() {
    let (changing, sender) = Changing::new();
    let policy = changing.filter(|_| true);
    let mut changes = Policy::changes(&policy).expect("a change stream");
    sender.try_send(()).expect("room");
    assert_eq!(block_on(changes.next()), Some(()));
}

#[test]
fn a_union_yields_when_either_side_changes() {
    let (left, left_sender) = Changing::new();
    let (right, right_sender) = Changing::new();
    let union = left.union(right);
    let mut changes = Policy::changes(&union).expect("a change stream");

    right_sender.try_send(()).expect("room");
    assert_eq!(block_on(changes.next()), Some(()));
    left_sender.try_send(()).expect("room");
    assert_eq!(block_on(changes.next()), Some(()));

    // One side ending leaves the other.
    drop(left_sender);
    right_sender.try_send(()).expect("room");
    assert_eq!(block_on(changes.next()), Some(()));
    drop(right_sender);
    assert_eq!(block_on(changes.next()), None);
}

#[test]
fn a_union_with_one_changing_side_yields_its_changes() {
    let (changing, sender) = Changing::new();
    let union = Open(Grant::everything()).union(changing);
    let mut changes = Policy::changes(&union).expect("a change stream");
    sender.try_send(()).expect("room");
    assert_eq!(block_on(changes.next()), Some(()));
    assert!(
        Policy::<Peer>::changes(&Policy::<Peer>::union(Open(Grant::everything()), Closed))
            .is_none()
    );
}

#[test]
fn an_erased_policy_forwards_decide_and_changes() {
    let (changing, sender) = Changing::new();
    let erased: Arc<dyn DynPolicy<Peer>> = Arc::new(Open(under("a")).union(changing));
    let decision = decide(&erased, &request("bob"));
    assert_eq!(decision.expect("admitted").grant, under("a"));

    let mut changes = Policy::changes(&erased).expect("a change stream");
    sender.try_send(()).expect("room");
    assert_eq!(block_on(changes.next()), Some(()));

    let closed: Arc<dyn DynPolicy<Peer>> = Arc::new(Closed);
    assert!(decide(&closed, &request("bob")).is_err());
    assert!(Policy::changes(&closed).is_none());
}

#[test]
fn transit_is_none_only_by_default() {
    assert!(Transit::default().is_none());
    assert!(
        !Transit {
            peer_forwards: true,
            ..Default::default()
        }
        .is_none()
    );
    assert!(
        !Transit {
            forward_from: Pattern::all().into(),
            ..Default::default()
        }
        .is_none()
    );
}

#[test]
fn the_default_grant_allows_nothing() {
    assert_eq!(Grant::default(), Grant::nothing());
    let authorization = Authorization::default();
    assert!(!authorization.grant.allows_subscribe("a"));
    assert!(authorization.transit.is_none());
    assert_eq!(authorization.expires, None);
}

#[test]
fn an_empty_stream_ends_a_union() {
    // A stream that ends at once does not hold the merged stream open.
    let union: BoxStream<()> = Box::pin(MergeBounded::from_iter([
        Box::pin(stream::empty()) as BoxStream<()>,
        Box::pin(stream::empty()),
    ]));
    assert_eq!(block_on(union.collect::<Vec<_>>()), Vec::<()>::new());
}
