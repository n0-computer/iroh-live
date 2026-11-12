use moq_net::Pattern;
use n0_future::future::block_on;

use super::*;

type Peer = String;

/// Returns a connect request of `peer` for the root, with no token.
fn request(peer: &str) -> PeerRequest<Peer> {
    PeerRequest::new(peer.to_owned(), SessionRequest::default(), Event::Connect)
}

/// Returns a grant that lets a peer publish under `prefix/**`.
fn under(prefix: &str) -> Grant {
    let pattern: Pattern = format!("{prefix}/**").parse().expect("a pattern");
    Grant::publish_under(pattern)
}

fn decide(policy: &impl Policy<Peer>, request: &PeerRequest<Peer>) -> Decision {
    block_on(policy.decide(request))
}

#[test]
fn a_request_reads_through_to_the_session_request() {
    let session = SessionRequest::new("/studio?jwt=abc&x=1");
    let request = PeerRequest::new("bob".to_owned(), session, Event::Reauthorize).with_dialed();
    assert_eq!(request.peer(), "bob");
    assert!(request.dialed());
    assert_eq!(request.event(), Event::Reauthorize);
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
        })
    });
    let joined = decide(&a.union(b), &request("bob")).expect("admitted");
    assert!(joined.grant.allows_publish("a/cam"));
    assert!(joined.grant.allows_publish("b/cam"));
    assert!(!joined.grant.allows_publish("c/cam"));
    assert!(joined.transit.forward_to.matches("any/path"));
    assert!(joined.transit.peer_forwards);
    assert_eq!(joined.expires, Some(soon));
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
fn an_erased_policy_forwards_decide() {
    let erased: Arc<dyn DynPolicy<Peer>> = Arc::new(Open(under("a")));
    let decision = decide(&erased, &request("bob"));
    assert_eq!(decision.expect("admitted").grant, under("a"));
    let closed: Arc<dyn DynPolicy<Peer>> = Arc::new(Closed);
    assert!(decide(&closed, &request("bob")).is_err());
}

#[test]
fn transit_is_none_only_by_default() {
    assert!(Transit::default().is_empty());
    assert!(
        !Transit {
            peer_forwards: true,
            ..Default::default()
        }
        .is_empty()
    );
    assert!(
        !Transit {
            forward_from: Pattern::all().into(),
            ..Default::default()
        }
        .is_empty()
    );
}

#[test]
fn the_default_grant_allows_nothing() {
    assert_eq!(Grant::default(), Grant::nothing());
    let authorization = Authorization::default();
    assert!(!authorization.grant.allows_subscribe("a"));
    assert!(authorization.transit.is_empty());
    assert_eq!(authorization.expires, None);
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
fn a_queue_asks_the_application_to_reauthorize() {
    let queue = Queue::new(4);
    let reauthorize = PeerRequest::new(
        "bob".to_owned(),
        SessionRequest::default(),
        Event::Reauthorize,
    );
    let decision = decide_queued(&queue, &reauthorize, |pending| {
        assert_eq!(pending.request().event(), Event::Reauthorize);
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
    PeerRequest::new(peer.to_owned(), SessionRequest::new(target), Event::Connect)
}

#[test]
fn a_good_token_admits_with_its_grant() {
    let policy = Verify::new(Stub);
    let decision = decide(&policy, &request_for("bob", "/?jwt=good"));
    assert_eq!(decision.expect("admitted").grant, under("bob"));
}

#[test]
fn a_bad_token_never_falls_through() {
    let policy = Verify::new(Stub).with_fallback(Open(Grant::everything()));
    let rejection = decide(&policy, &request_for("bob", "/?jwt=forged")).expect_err("rejected");
    assert_eq!(rejection.reason(), "bad token");
    assert!(matches!(rejection.code(), moq_net::Error::App(3)));
}

#[test]
fn no_token_asks_the_fallback() {
    let policy = Verify::new(Stub).with_fallback(Open(Grant::subscribe_only()));
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
    let policy = Verify::new(Lapsed).with_fallback(Open(Grant::subscribe_only()));
    let session = SessionRequest::new("/?jwt=old");
    let connect = PeerRequest::new("bob".to_owned(), session.clone(), Event::Connect);
    let rejection = decide(&policy, &connect).expect_err("a new session is rejected");
    assert!(matches!(rejection.code(), moq_net::Error::Expired));
    let again = PeerRequest::new("bob".to_owned(), session, Event::Reauthorize);
    assert_eq!(
        decide(&policy, &again).expect("narrowed").grant,
        Grant::subscribe_only()
    );
    // Under the default fallback, the session closes.
    decide(&Verify::new(Lapsed), &again).expect_err("closed");
    // A token rejected for another reason never falls through.
    let forged = PeerRequest::new(
        "bob".to_owned(),
        SessionRequest::new("/?jwt=forged"),
        Event::Reauthorize,
    );
    let policy = Verify::new(Stub).with_fallback(Open(Grant::everything()));
    decide(&policy, &forged).expect_err("rejected");
}

#[test]
fn debug_output_never_shows_a_token() {
    let session = SessionRequest::new("/space?jwt=secret-query&mode=x").with_token("secret-setup");
    let request = PeerRequest::new("bob".to_owned(), session.clone(), Event::Connect);
    let queue = Queue::new(1);
    let mut deciding = std::pin::pin!(Policy::decide(&queue, &request));
    assert!(
        block_on(future::poll_once(&mut deciding)).is_none(),
        "the request waits"
    );
    let pending = block_on(queue.next()).expect("a queued request");
    for printed in [
        format!("{session:?}"),
        format!("{request:?}"),
        format!("{pending:?}"),
        format!("{:?}", SessionRequest::new("/a?jwt=secret")),
    ] {
        assert!(!printed.contains("secret"), "{printed}");
    }
    assert!(
        format!("{session:?}").contains("mode"),
        "query keys are shown"
    );
}

/// A policy that never decides.
#[derive(Debug)]
struct Never;

impl Policy<Peer> for Never {
    async fn decide(&self, _request: &PeerRequest<Peer>) -> Decision {
        future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn a_timeout_rejects_a_slow_policy_with_the_timeout_code() {
    let policy = Never.timeout(Duration::from_secs(5));
    let rejection = Policy::decide(&policy, &request("bob"))
        .await
        .expect_err("timed out");
    assert!(
        matches!(rejection.code(), moq_net::Error::Timeout),
        "{rejection}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_timeout_passes_a_fast_decision_through() {
    let admitted = Policy::decide(
        &Policy::<Peer>::timeout(Open(under("a")), Duration::from_secs(5)),
        &request("bob"),
    )
    .await
    .expect("admitted");
    assert_eq!(admitted.grant, under("a"));
    let closed = Policy::<Peer>::timeout(Closed, Duration::from_secs(5));
    let rejection = Policy::decide(&closed, &request("bob"))
        .await
        .expect_err("rejected");
    assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
}

#[tokio::test(start_paused = true)]
async fn a_union_admits_on_its_fast_half_when_the_slow_half_times_out() {
    let policy = Open(under("a")).union(Never.timeout(Duration::from_secs(5)));
    let admitted = Policy::decide(&policy, &request("bob"))
        .await
        .expect("admitted");
    assert_eq!(admitted.grant, under("a"));
}

#[tokio::test(start_paused = true)]
async fn decide_within_passes_an_admission_through() {
    let admitted = decide_within(&Open(under("a")), &request("bob"), Duration::from_secs(5))
        .await
        .expect("admitted");
    assert_eq!(admitted, Authorization::from(under("a")));
}

#[tokio::test(start_paused = true)]
async fn decide_within_rejects_a_policy_that_takes_too_long() {
    let rejection = decide_within(&Never, &request("bob"), Duration::from_secs(5))
        .await
        .expect_err("timed out");
    assert!(
        matches!(rejection.code(), moq_net::Error::Timeout),
        "{rejection}"
    );
}

#[tokio::test(start_paused = true)]
async fn decide_within_keeps_the_policys_own_rejection() {
    let policy = from_fn(|_: &PeerRequest<Peer>| {
        Err(Rejection::new("banned").with_code(moq_net::Error::App(9)))
    });
    let rejection = decide_within(&policy, &request("bob"), Duration::from_secs(5))
        .await
        .expect_err("rejected");
    assert_eq!(rejection.reason(), "banned");
    assert!(matches!(rejection.code(), moq_net::Error::App(9)));
}

#[tokio::test(start_paused = true)]
async fn decide_within_rejects_an_authorization_that_asks_for_transit() {
    let policy = from_fn(|_: &PeerRequest<Peer>| {
        Ok(Authorization {
            transit: Transit {
                peer_forwards: true,
                ..Default::default()
            },
            ..Grant::everything().into()
        })
    });
    let rejection = decide_within(&policy, &request("bob"), Duration::from_secs(5))
        .await
        .expect_err("rejected");
    assert!(rejection.reason().contains("transit"), "{rejection}");
    assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
}

#[test]
fn a_queue_frees_its_slot_once_the_application_takes_a_request() {
    let queue = Queue::new(1);
    let bob = request("bob");
    let mut waiting = Box::pin(Policy::decide(&queue, &bob));
    assert!(block_on(future::poll_once(&mut waiting)).is_none());
    let pending = block_on(queue.next()).expect("bob's request");
    // The request left the queue, so another fits while bob's is answered.
    let decision = decide_queued(&queue, &request("carol"), |pending| {
        pending.admit(under("carol"));
    });
    assert_eq!(decision.expect("admitted").grant, under("carol"));
    pending.admit(under("bob"));
    assert_eq!(block_on(waiting).expect("admitted").grant, under("bob"));
}

#[test]
fn closing_a_queue_still_hands_out_the_waiting_requests() {
    let queue = Queue::new(4);
    let bob = request("bob");
    let mut waiting = Box::pin(Policy::decide(&queue, &bob));
    assert!(block_on(future::poll_once(&mut waiting)).is_none());
    queue.close();
    block_on(queue.next())
        .expect("bob's request outlives the close")
        .admit(Grant::subscribe_only());
    assert!(block_on(waiting).is_ok());
    assert!(block_on(queue.next()).is_none());
}

#[test]
fn an_answer_after_the_decision_was_dropped_is_ignored() {
    let queue = Queue::new(4);
    let bob = request("bob");
    let mut waiting = Box::pin(Policy::decide(&queue, &bob));
    assert!(block_on(future::poll_once(&mut waiting)).is_none());
    drop(waiting);
    // Nobody waits for the answer any more, which is no error.
    block_on(queue.next())
        .expect("bob's request")
        .admit(Grant::everything());
}

#[test]
fn a_fallback_rejection_keeps_its_code() {
    let fallback = from_fn(|_: &PeerRequest<Peer>| {
        Err(Rejection::new("strangers").with_code(moq_net::Error::App(4)))
    });
    let policy = Verify::new(Stub).with_fallback(fallback);
    let rejection = decide(&policy, &request_for("bob", "/")).expect_err("rejected");
    assert_eq!(rejection.reason(), "strangers");
    assert!(matches!(rejection.code(), moq_net::Error::App(4)));
}

#[test]
fn a_good_token_keeps_its_expiry() {
    #[derive(Debug)]
    struct Expiring(SystemTime);

    impl TokenVerifier<Peer> for Expiring {
        fn verify(&self, _token: &str, _request: &PeerRequest<Peer>) -> Decision {
            Ok(Authorization {
                expires: Some(self.0),
                ..Grant::everything().into()
            })
        }
    }

    let expires = SystemTime::UNIX_EPOCH + Duration::from_secs(4_000_000_000);
    let policy = Verify::new(Expiring(expires)).with_fallback(Open(Grant::subscribe_only()));
    let authorization = decide(&policy, &request_for("bob", "/?jwt=any")).expect("admitted");
    assert_eq!(authorization.expires, Some(expires));
    assert_eq!(authorization.grant, Grant::everything());
}
