//! What a transport tells a player about the link it plays over.
//!
//! A transport attaches a function returning a [`NetworkSample`] to a
//! [`RemoteBroadcast`](crate::RemoteBroadcast) with
//! [`set_network`](crate::RemoteBroadcast::set_network). Every player of that
//! broadcast calls it a few times a second to choose a rendition.

use std::sync::Arc;

pub use moq_ext_adaptive::NetworkSample;

/// A transport's view of the link, attached with [`crate::RemoteBroadcast::set_network`].
pub(crate) type NetworkSignals = Arc<dyn Fn() -> NetworkSample + Send + Sync>;
