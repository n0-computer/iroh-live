//! Policies that decide who may hold a session with a node, and with which rights.
//!
//! The types are [`moq_ext_access::policy`]'s, with the peer fixed to an
//! iroh [`EndpointId`]. Set a node's policy with
//! [`NodeConfig::with_policy`](crate::NodeConfig::with_policy). The node asks
//! it when it accepts a session, after the peer's MoQ SETUP, and before it
//! dials a peer. All sessions with one peer share the authorization the
//! policy gave the first of them.
//!
//! To decide in the application, as a person who answers a call does, use a
//! [`Queue`] and answer each [`Pending`] it yields. Raise
//! [`NodeConfig::decide_timeout`](crate::NodeConfig::decide_timeout) then,
//! since the node rejects a session whose decision takes longer.
//!
//! To admit the peers that present a token, wrap a [`TokenVerifier`] in a
//! [`Verify`]. The `jwt` feature adds `JwtVerifier`, for moq-relay's JWTs,
//! and the `rcan` feature adds `RcanVerifier`, for capability chains bound to
//! the peer's key.
//!
//! # Examples
//!
//! Admit only known peers, and let each publish under its own id:
//!
//! ```
//! use std::collections::BTreeSet;
//!
//! use iroh::EndpointId;
//! use iroh_live_node::{
//!     Grant, NodeConfig,
//!     net::Pattern,
//!     policy::{self, PeerRequest, Rejection},
//! };
//!
//! let known: BTreeSet<EndpointId> = BTreeSet::new();
//! let config =
//!     NodeConfig::default().with_policy(policy::from_fn(move |request: &PeerRequest| {
//!         let peer = request.peer();
//!         if !known.contains(peer) {
//!             return Err(Rejection::new("unknown peer"));
//!         }
//!         let own: Pattern = format!("live/{peer}/**").parse().expect("a pattern");
//!         Ok(Grant::publish_under(own).into())
//!     }));
//! # drop(config);
//! ```

use iroh::EndpointId;
#[cfg(feature = "jwt")]
pub use moq_ext_access::policy::JwtVerifier;
pub use moq_ext_access::policy::{
    Authorization, Closed, Decision, DynPolicy, Event, Filter, FromFn, Open, Policy, Rejection,
    Timeout, TokenVerifier, Transit, Union, Verify, from_fn,
};
#[cfg(feature = "rcan")]
pub use moq_ext_access::policy::{MoqCapability, RcanVerifier, encode_chain};

/// A peer's request for a session with this node, as a policy sees it.
pub type PeerRequest = moq_ext_access::policy::PeerRequest<EndpointId>;

/// A policy that hands each decision about a peer to the application.
pub type Queue = moq_ext_access::policy::Queue<EndpointId>;

/// A request about a peer, waiting in a [`Queue`] for the application's answer.
pub type Pending = moq_ext_access::policy::Pending<EndpointId>;
