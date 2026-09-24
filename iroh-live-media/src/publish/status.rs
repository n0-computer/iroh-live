//! The watched state of a broadcast's slots and renditions.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use n0_watcher::Watchable;

use crate::error::Error;

/// The state of one media slot, on either side: a broadcast's video or audio,
/// or a player's.
///
/// Shared by [`PublishStatus`] and [`PlayerStatus`](crate::PlayerStatus), so a
/// UI reports both the same way. Two `Failed` states are equal only when they
/// hold the same error, which is what a watcher needs to tell an update from
/// a repeat.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum SlotState {
    /// Nothing is set.
    #[default]
    Off,
    /// Opening, probing encoders, or waiting for a first keyframe.
    Starting,
    /// Media is flowing.
    Running,
    /// Stopped by an error.
    ///
    /// Nothing retries except the documented hardware-to-software encoder
    /// fallback, and a microphone, which moq-audio reopens on its own when the
    /// device returns.
    Failed(Arc<Error>),
    /// The source ended.
    Ended,
}

impl PartialEq for SlotState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Off, Self::Off)
            | (Self::Starting, Self::Starting)
            | (Self::Running, Self::Running)
            | (Self::Ended, Self::Ended) => true,
            (Self::Failed(left), Self::Failed(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl Eq for SlotState {}

/// Whether one rendition is encoding.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum RenditionState {
    /// Advertised, with nobody watching, so nothing is encoded.
    #[default]
    Idle,
    /// Somebody is watching, and this encoder is running.
    Encoding {
        /// The encoder backend, such as `openh264` or `vaapi`.
        encoder: String,
    },
    /// Its encoder failed, and the fallback too if there was one.
    Failed(Arc<Error>),
}

impl PartialEq for RenditionState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Idle, Self::Idle) => true,
            (Self::Encoding { encoder: left }, Self::Encoding { encoder: right }) => left == right,
            (Self::Failed(left), Self::Failed(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl Eq for RenditionState {}

/// The state of both slots of a broadcast, and of every rendition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PublishStatus {
    /// The video slot.
    pub video: SlotState,
    /// The audio slot.
    pub audio: SlotState,
    /// Per rendition, video and audio alike, by track name: whether anyone is
    /// watching it, and so whether it encodes.
    pub renditions: BTreeMap<String, RenditionState>,
}

/// Which slot a report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Medium {
    Video,
    Audio,
}

/// The status, and the generation and rendition names of the bundle that owns
/// each slot.
#[derive(Debug, Default)]
struct Guarded {
    status: PublishStatus,
    video: Owner,
    audio: Owner,
}

/// Which bundle owns a slot.
#[derive(Debug, Default)]
struct Owner {
    generation: u64,
    names: Vec<String>,
}

impl Guarded {
    fn owner(&mut self, medium: Medium) -> &mut Owner {
        match medium {
            Medium::Video => &mut self.video,
            Medium::Audio => &mut self.audio,
        }
    }

    fn state(&mut self, medium: Medium) -> &mut SlotState {
        match medium {
            Medium::Video => &mut self.status.video,
            Medium::Audio => &mut self.status.audio,
        }
    }
}

/// The broadcast's status, written only by the task whose slot bundle is
/// current.
///
/// A replaced publish task can still be finishing when its replacement starts,
/// and a report from it would overwrite the new slot's state. Every write goes
/// through a [`Reporter`] carrying the generation of the bundle it belongs to,
/// and is dropped unless that generation is still the slot's, checked under the
/// same lock the write takes.
#[derive(Debug, Clone)]
pub(crate) struct StatusCell {
    guarded: Arc<Mutex<Guarded>>,
    watch: Watchable<PublishStatus>,
}

impl Default for StatusCell {
    fn default() -> Self {
        Self {
            guarded: Default::default(),
            watch: Watchable::new(PublishStatus::default()),
        }
    }
}

impl StatusCell {
    /// Returns a watcher over the status.
    pub(crate) fn watch(&self) -> n0_watcher::Direct<PublishStatus> {
        self.watch.watch()
    }

    /// Returns the current status.
    #[cfg(test)]
    fn get(&self) -> PublishStatus {
        self.watch.get()
    }

    /// Hands `medium` to a new bundle of `generation`, replacing what the old
    /// one reported, and returns the new bundle's reporter.
    pub(crate) fn begin(&self, medium: Medium, generation: u64, names: &[String]) -> Reporter {
        let mut guarded = self.guarded.lock().expect("poisoned");
        let previous = std::mem::replace(
            guarded.owner(medium),
            Owner {
                generation,
                names: names.to_vec(),
            },
        );
        for name in &previous.names {
            guarded.status.renditions.remove(name);
        }
        *guarded.state(medium) = SlotState::Starting;
        for name in names {
            guarded
                .status
                .renditions
                .insert(name.clone(), RenditionState::Idle);
        }
        self.watch.set(guarded.status.clone()).ok();
        Reporter {
            cell: self.clone(),
            medium,
            generation,
            names: names.to_vec(),
        }
    }

    /// Clears `medium` to `Off`, provided the bundle of `generation` still
    /// owns it.
    pub(crate) fn clear(&self, medium: Medium, generation: u64) {
        let mut guarded = self.guarded.lock().expect("poisoned");
        if guarded.owner(medium).generation != generation {
            return;
        }
        let names = std::mem::take(&mut guarded.owner(medium).names);
        for name in &names {
            guarded.status.renditions.remove(name);
        }
        *guarded.state(medium) = SlotState::Off;
        self.watch.set(guarded.status.clone()).ok();
    }

    fn write(&self, medium: Medium, generation: u64, f: impl FnOnce(&mut PublishStatus)) {
        let mut guarded = self.guarded.lock().expect("poisoned");
        if guarded.owner(medium).generation != generation {
            return;
        }
        f(&mut guarded.status);
        self.watch.set(guarded.status.clone()).ok();
    }
}

/// Writes one slot bundle's state into a [`StatusCell`].
#[derive(Debug, Clone)]
pub(crate) struct Reporter {
    cell: StatusCell,
    medium: Medium,
    generation: u64,
    names: Vec<String>,
}

impl Reporter {
    /// Sets the slot's state.
    pub(crate) fn slot(&self, state: SlotState) {
        let medium = self.medium;
        self.cell
            .write(medium, self.generation, |status| match medium {
                Medium::Video => status.video = state,
                Medium::Audio => status.audio = state,
            });
    }

    /// Sets one rendition's state.
    pub(crate) fn rendition(&self, name: &str, state: RenditionState) {
        if !self.names.iter().any(|owned| owned == name) {
            return;
        }
        self.cell.write(self.medium, self.generation, |status| {
            status.renditions.insert(name.to_string(), state);
        });
    }
}

#[cfg(test)]
mod tests {
    use n0_watcher::Watcher as _;

    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// R07's status half: a replaced publish that is still winding down must
    /// not overwrite what its replacement reports.
    #[test]
    fn a_stale_bundle_cannot_write() {
        let cell = StatusCell::default();
        let old = cell.begin(Medium::Video, 1, &names(&["video"]));
        let new = cell.begin(Medium::Video, 2, &names(&["video"]));
        new.slot(SlotState::Running);
        old.slot(SlotState::Ended);
        old.rendition(
            "video",
            RenditionState::Encoding {
                encoder: "stale".into(),
            },
        );
        let status = cell.get();
        assert_eq!(status.video, SlotState::Running);
        assert_eq!(status.renditions["video"], RenditionState::Idle);
    }

    #[test]
    fn a_new_bundle_replaces_the_old_renditions_only_for_its_medium() {
        let cell = StatusCell::default();
        let _audio = cell.begin(Medium::Audio, 1, &names(&["opus"]));
        let _video = cell.begin(Medium::Video, 1, &names(&["360p", "720p"]));
        let _video = cell.begin(Medium::Video, 2, &names(&["1080p"]));
        let status = cell.get();
        let listed: Vec<&str> = status.renditions.keys().map(String::as_str).collect();
        assert_eq!(listed, ["1080p", "opus"]);
        assert_eq!(status.video, SlotState::Starting);
    }

    #[test]
    fn clearing_turns_the_slot_off() {
        let cell = StatusCell::default();
        let video = names(&["video"]);
        let reporter = cell.begin(Medium::Video, 1, &video);
        reporter.slot(SlotState::Running);
        cell.clear(Medium::Video, 1);
        let status = cell.get();
        assert_eq!(status.video, SlotState::Off);
        assert!(status.renditions.is_empty());
    }

    #[test]
    fn failures_compare_by_identity() {
        let err = Arc::new(n0_error::e!(Error::Closed));
        assert_eq!(SlotState::Failed(err.clone()), SlotState::Failed(err));
        assert_ne!(
            SlotState::Failed(Arc::new(n0_error::e!(Error::Closed))),
            SlotState::Failed(Arc::new(n0_error::e!(Error::Closed)))
        );
    }

    #[test]
    fn the_watcher_sees_each_change() {
        let cell = StatusCell::default();
        let mut watch = cell.watch();
        let reporter = cell.begin(Medium::Audio, 1, &names(&["opus"]));
        assert_eq!(watch.get().audio, SlotState::Starting);
        reporter.slot(SlotState::Running);
        assert_eq!(watch.get().audio, SlotState::Running);
    }
}
