//! Policies that decide who may hold a session with a node, and with which rights.
//!
//! The types are [`moq_ext_access::policy`]'s, with the peer fixed to an
//! iroh [`EndpointId`]. Set a node's policy with
//! [`NodeConfig::with_policy`](crate::NodeConfig::with_policy). The node asks
//! it when it accepts a session, after the peer's MoQ SETUP, and before it
//! dials a peer. All sessions with one peer share the authorization the
//! policy gave the first of them.
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
pub use moq_ext_access::policy::{
    Authorization, Closed, Decision, DynPolicy, Event, Filter, FromFn, Open, Policy, Rejection,
    Transit, Union, from_fn,
};

/// A peer's request for a session with this node, as a policy sees it.
pub type PeerRequest = moq_ext_access::policy::PeerRequest<EndpointId>;
