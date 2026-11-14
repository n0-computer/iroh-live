//! moq-net's send groups, mapped onto QUIC stream urgency.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use iroh::endpoint;
use moq_net::transport::poll;

/// The tiers inside one group. Streams past the last share it.
const TIERS: i32 = 16;

/// The bound a group priority is clamped to, either way.
///
/// moq-net packs a subscriber and a publisher `i8` into
/// `subscriber * 256 + publisher`, well inside it.
const GROUP_PRIORITY_LIMIT: i64 = 1 << 20;

/// Where ungrouped streams start, above the highest grouped urgency.
///
/// Grouped urgencies stay within `GROUP_PRIORITY_LIMIT * TIERS`, 2^24 either way.
const UNGROUPED_BASE: i32 = 1 << 29;

/// The bound an ungrouped priority is clamped to, either way, which keeps every
/// ungrouped urgency above the grouped ones and inside `i32`.
const UNGROUPED_LIMIT: i64 = (1 << 28) - 1;

/// Returns the urgency of a stream outside every group at `priority`.
pub(crate) fn ungrouped(priority: i64) -> i32 {
    // Within 2^28 - 1 of 2^29 either way, so inside (2^28, 2^30).
    UNGROUPED_BASE + priority.clamp(-UNGROUPED_LIMIT, UNGROUPED_LIMIT) as i32
}

/// Returns the urgency of a stream at `tier` of a group at `priority`.
fn grouped(priority: i64, tier: usize) -> i32 {
    let priority = priority.clamp(-GROUP_PRIORITY_LIMIT, GROUP_PRIORITY_LIMIT) as i32;
    let tier = i32::try_from(tier).unwrap_or(i32::MAX).min(TIERS - 1);
    priority * TIERS + (TIERS - 1 - tier)
}

/// A QUIC send stream that a group and its owner both reach.
///
/// The owner writes through it and the group sets its urgency, which happens
/// whenever another stream of the group joins, leaves or moves.
pub(crate) type SharedStream = Arc<Mutex<endpoint::SendStream>>;

/// Locks `stream`, recovering it from a panic in another holder.
///
/// Every holder leaves the stream valid, so a poisoned lock is safe to use.
pub(crate) fn lock(stream: &SharedStream) -> MutexGuard<'_, endpoint::SendStream> {
    stream.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A send group, from [`send_group`](poll::Session::send_group) on a
/// [`Transport`](crate::Transport).
///
/// moq-net opens one send group per subscription and ranks its streams in two
/// levels: a higher-priority group preempts every lower one, groups of equal
/// priority share the bytes, and inside a group streams go in their own order,
/// the newest group of a track first. Streams outside every group, the control
/// streams, go before all of them. moq-quic schedules exactly that, with
/// deficit round robin between groups.
///
/// iroh's QUIC stack has one level instead: an `i32` urgency per stream,
/// higher first. Streams of one urgency are sent round robin while the
/// connection's send fairness is on, which is noq's default, so they share the
/// bytes. The two levels are packed into the urgency:
///
/// ```text
/// ungrouped:  2^29 + priority                          above every group
/// grouped:    group priority * 16 + (15 - tier)
/// ```
///
/// A stream's tier is its dense rank inside its group: the streams with the
/// highest order in the group are tier 0, the next distinct order tier 1, up
/// to tier 15, which the rest share. So the streams heading equal-priority
/// groups share one urgency and split the bytes, a group's older streams wait
/// for its head, and a higher-priority group outranks every tier of a lower
/// one. A stream joining, leaving or moving reranks its group, and iroh applies
/// a new urgency to queued data at the stream's next turn.
///
/// It departs from moq-quic in one way: fairness is per stream and only between
/// streams of one tier. When the head of a group has nothing queued, the next
/// stream of that group waits behind the heads of the other groups rather than
/// taking the idle head's share. For live media that sheds a group's backlog
/// first, as the newest-first order asks anyway.
///
/// The sharing rests on the connection's send fairness: with
/// `send_fairness(false)` in iroh's transport config, streams of one urgency go
/// in the order they were written to, and the older of two equal-priority
/// groups goes first again. RFC 9218's per-stream incremental flag, which
/// [n0-computer/noq#859](https://github.com/n0-computer/noq/pull/859) adds,
/// would let each stream ask for round robin whatever that setting.
///
/// Clones share the group. It lives as long as a handle or a member stream
/// does.
#[derive(Clone)]
pub struct SendGroup {
    state: Arc<Mutex<GroupState>>,
}

impl std::fmt::Debug for SendGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("SendGroup")
            .field("priority", &state.priority)
            .field("members", &state.members.len())
            .finish()
    }
}

#[derive(Debug)]
struct GroupState {
    priority: i64,
    /// The id the next member gets.
    next: u64,
    members: Vec<Member>,
}

#[derive(Debug)]
struct Member {
    id: u64,
    /// The stream's order inside the group: higher goes first.
    order: i64,
    stream: SharedStream,
    /// The urgency last set on the stream, so a rerank touches only the
    /// streams whose urgency moved.
    urgency: Option<i32>,
}

/// A stream's place in a group, which it leaves on drop.
#[derive(Debug)]
pub(crate) struct Membership {
    group: SendGroup,
    id: u64,
}

impl SendGroup {
    pub(crate) fn new(priority: i64) -> Self {
        Self {
            state: Arc::new(Mutex::new(GroupState {
                priority,
                next: 0,
                members: Vec::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, GroupState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds `stream` to the group at `order` and reranks the group.
    pub(crate) fn join(&self, stream: SharedStream, order: i64) -> Membership {
        let mut state = self.lock();
        let id = state.next;
        state.next += 1;
        state.members.push(Member {
            id,
            order,
            stream,
            urgency: None,
        });
        state.rerank();
        Membership {
            group: self.clone(),
            id,
        }
    }

    /// Returns whether `self` and `other` are the same group.
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl poll::SendGroup for SendGroup {
    fn set_priority(&mut self, priority: i64) {
        let mut state = self.lock();
        if state.priority != priority {
            state.priority = priority;
            state.rerank();
        }
    }
}

impl Membership {
    /// Returns the group the stream is in.
    pub(crate) fn group(&self) -> &SendGroup {
        &self.group
    }

    /// Moves the stream to `order` inside its group.
    pub(crate) fn set_order(&self, order: i64) {
        let mut state = self.group.lock();
        if let Some(member) = state.members.iter_mut().find(|m| m.id == self.id) {
            member.order = order;
        }
        state.rerank();
    }
}

impl Drop for Membership {
    /// Leaves the group, promoting the streams behind this one.
    ///
    /// Whatever the stream still has queued keeps the urgency it had.
    fn drop(&mut self) {
        let mut state = self.group.lock();
        state.members.retain(|m| m.id != self.id);
        state.rerank();
    }
}

impl GroupState {
    /// Sets every member's urgency from its tier and the group's priority.
    ///
    /// iroh applies a new urgency to queued data at the stream's next turn.
    fn rerank(&mut self) {
        let mut orders: Vec<i64> = self.members.iter().map(|m| m.order).collect();
        // Highest order first, ties sharing a tier.
        orders.sort_unstable_by(|a, b| b.cmp(a));
        orders.dedup();
        for member in &mut self.members {
            let tier = orders
                .binary_search_by(|order| member.order.cmp(order))
                .unwrap_or_else(|tier| tier);
            let urgency = grouped(self.priority, tier);
            if member.urgency != Some(urgency) {
                // Fails only once the stream is closed, when the urgency no
                // longer matters.
                lock(&member.stream).set_priority(urgency).ok();
                member.urgency = Some(urgency);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ungrouped_outranks_every_group() {
        let lowest = ungrouped(i64::MIN);
        assert!(lowest > grouped(i64::MAX, 0));
        assert!(lowest > 0);
        assert!(ungrouped(1) > ungrouped(0));
        assert_eq!(ungrouped(i64::MAX), UNGROUPED_BASE + UNGROUPED_LIMIT as i32);
    }

    #[test]
    fn a_higher_group_outranks_every_tier_of_a_lower_one() {
        assert!(grouped(1, TIERS as usize) > grouped(0, 0));
        assert!(grouped(-1, 0) < grouped(0, usize::MAX));
        // moq-net's packing: a subscriber priority step dominates the publisher's.
        let packed = |subscriber: i64, publisher: i64| subscriber * 256 + publisher;
        assert!(grouped(packed(1, -128), 99) > grouped(packed(0, 127), 0));
    }

    #[test]
    fn tiers_order_within_a_group_and_saturate() {
        assert!(grouped(3, 0) > grouped(3, 1));
        assert_eq!(grouped(3, TIERS as usize - 1), grouped(3, 1000));
        assert_eq!(grouped(i64::MIN, 0), grouped(-GROUP_PRIORITY_LIMIT, 0));
    }
}
