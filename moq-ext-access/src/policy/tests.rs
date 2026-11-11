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

/// Decides `request` with `queue` while `answer` handles the queued request.
fn decide_queued(
    queue: &Queue<Peer>,
    request: &PeerRequest<Peer>,
    answer: impl FnOnce(Pending<Peer>),
) -> Decision {
    let answering = async {
        let pending = queue.next().await.expect("a queued request");
        answer(pending);
    };
    block_on(future::zip(Policy::decide(queue, request), answering)).0
}

#[test]
fn a_queue_admits_what_the_application_admits() {
    let queue = Queue::new(4);
    let decision = decide_queued(&queue, &request("bob"), |pending| {
        assert_eq!(pending.request().peer(), "bob");
        pending.admit(under("bob"));
    });
    assert_eq!(decision.expect("admitted").grant, under("bob"));
}

#[test]
fn a_queue_rejects_what_the_application_rejects() {
    let queue = Queue::new(4);
    let decision = decide_queued(&queue, &request("bob"), |pending| {
        pending.reject(Rejection::new("no").with_code(moq_net::Error::App(7)));
    });
    let rejection = decision.expect_err("rejected");
    assert_eq!(rejection.reason(), "no");
    assert!(matches!(rejection.code(), moq_net::Error::App(7)));
}

#[test]
fn a_dropped_pending_rejects() {
    let queue = Queue::new(4);
    let decision = decide_queued(&queue, &request("bob"), drop);
    assert_eq!(decision.expect_err("rejected").reason(), "not answered");
}

#[test]
fn a_full_queue_rejects_at_once() {
    let queue = Queue::new(1);
    // The first request waits in the queue, unanswered.
    let bob = request("bob");
    let mut waiting = Box::pin(Policy::decide(&queue, &bob));
    assert!(block_on(future::poll_once(&mut waiting)).is_none());
    let rejection = decide(&queue, &request("carol")).expect_err("rejected");
    assert!(matches!(rejection.code(), moq_net::Error::TooManyRequests));
    // The first one is still answered.
    block_on(queue.next())
        .expect("bob's request")
        .admit(Grant::everything());
    assert!(block_on(waiting).is_ok());
}

#[test]
fn a_closed_queue_rejects_and_ends() {
    let queue = Queue::<Peer>::new(4);
    queue.close();
    let rejection = decide(&queue, &request("bob")).expect_err("rejected");
    assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
    assert!(block_on(queue.next()).is_none());
}

#[test]
fn a_queue_keeps_the_authorization_at_a_revalidation() {
    let queue = Queue::new(4);
    let current = Authorization {
        revalidate: Some(Duration::from_secs(5)),
        ..under("bob").into()
    };
    let revalidate = PeerRequest::new(
        "bob".to_owned(),
        SessionRequest::default(),
        false,
        Event::Revalidate,
    )
    .with_current(current.clone());
    assert_eq!(decide(&queue, &revalidate).expect("kept"), current);
    assert!(
        block_on(future::poll_once(queue.next())).is_none(),
        "the application saw a revalidation"
    );
}

#[test]
fn a_queue_asks_the_application_to_reauthorize() {
    let queue = Queue::new(4);
    let reauthorize = PeerRequest::new(
        "bob".to_owned(),
        SessionRequest::default(),
        false,
        Event::Reauthorize,
    )
    .with_current(under("bob").into());
    let decision = decide_queued(&queue, &reauthorize, |pending| {
        assert_eq!(pending.request().event(), Event::Reauthorize);
        assert_eq!(
            pending.request().current().map(|current| &current.grant),
            Some(&under("bob"))
        );
        pending.admit(Grant::subscribe_only());
    });
    assert_eq!(decision.expect("admitted").grant, Grant::subscribe_only());
}

/// Admits the token `good` with a grant under the peer's name, and rejects any other.
#[derive(Debug)]
struct Stub;

impl TokenVerifier<Peer> for Stub {
    fn verify(&self, token: &str, request: &PeerRequest<Peer>) -> Decision {
        match token {
            "good" => Ok(under(request.peer()).into()),
            _ => Err(Rejection::new("bad token").with_code(moq_net::Error::App(3))),
        }
    }
}

/// Returns a connect request of `peer` for `target`, such as `/?jwt=good`.
fn request_for(peer: &str, target: &str) -> PeerRequest<Peer> {
    PeerRequest::new(
        peer.to_owned(),
        SessionRequest::new(target),
        false,
        Event::Connect,
    )
}

#[test]
fn a_good_token_admits_with_its_grant() {
    let policy = Verify::new(Stub);
    let decision = decide(&policy, &request_for("bob", "/?jwt=good"));
    assert_eq!(decision.expect("admitted").grant, under("bob"));
}

#[test]
fn a_bad_token_never_falls_through() {
    let policy = Verify::new(Stub).fallback(Open(Grant::everything()));
    let rejection = decide(&policy, &request_for("bob", "/?jwt=forged")).expect_err("rejected");
    assert_eq!(rejection.reason(), "bad token");
    assert!(matches!(rejection.code(), moq_net::Error::App(3)));
}

#[test]
fn no_token_asks_the_fallback() {
    let policy = Verify::new(Stub).fallback(Open(Grant::subscribe_only()));
    let decision = decide(&policy, &request_for("bob", "/"));
    assert_eq!(decision.expect("admitted").grant, Grant::subscribe_only());
}

#[test]
fn the_default_fallback_requires_a_token() {
    let policy = Verify::new(Stub);
    let rejection = decide(&policy, &request_for("bob", "/")).expect_err("rejected");
    assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
}

#[test]
fn a_token_query_is_read_too() {
    assert_eq!(SessionRequest::new("/a?token=t1").token(), Some("t1"));
    // `jwt` wins over `token`, and a setup token over both.
    assert_eq!(
        SessionRequest::new("/a?token=t1&jwt=j1").token(),
        Some("j1")
    );
    assert_eq!(
        SessionRequest::new("/a?token=t1").with_token("s1").token(),
        Some("s1")
    );
    let policy = Verify::new(Stub);
    assert!(decide(&policy, &request_for("bob", "/?token=good")).is_ok());
}

#[test]
fn verify_takes_its_changes_from_the_fallback() {
    let (changing, sender) = Changing::new();
    let policy = Verify::new(Stub).fallback(changing);
    let mut changes = Policy::<Peer>::changes(&policy).expect("a change stream");
    sender.try_send(()).expect("room");
    assert_eq!(block_on(changes.next()), Some(()));
    assert!(Policy::<Peer>::changes(&Verify::new(Stub)).is_none());
}

/// Rejects every token as expired.
#[derive(Debug)]
struct Lapsed;

impl TokenVerifier<Peer> for Lapsed {
    fn verify(&self, _token: &str, _request: &PeerRequest<Peer>) -> Decision {
        Err(Rejection::new("expired").with_code(moq_net::Error::Expired))
    }
}

#[test]
fn an_expired_token_falls_through_only_when_deciding_again() {
    let policy = Verify::new(Lapsed).fallback(Open(Grant::subscribe_only()));
    let session = SessionRequest::new("/?jwt=old");
    let connect = PeerRequest::new("bob".to_owned(), session.clone(), false, Event::Connect);
    let rejection = decide(&policy, &connect).expect_err("a new session is rejected");
    assert!(matches!(rejection.code(), moq_net::Error::Expired));
    for event in [Event::Revalidate, Event::Reauthorize] {
        let again = PeerRequest::new("bob".to_owned(), session.clone(), false, event);
        assert_eq!(
            decide(&policy, &again).expect("narrowed").grant,
            Grant::subscribe_only()
        );
        // Under the default fallback, the session closes.
        decide(&Verify::new(Lapsed), &again).expect_err("closed");
    }
    // A token rejected for another reason never falls through.
    let forged = PeerRequest::new(
        "bob".to_owned(),
        SessionRequest::new("/?jwt=forged"),
        false,
        Event::Reauthorize,
    );
    let policy = Verify::new(Stub).fallback(Open(Grant::everything()));
    decide(&policy, &forged).expect_err("rejected");
}
