//! The watched state of a broadcast's slots and renditions.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use n0_watcher::Watchable;

use crate::error::Error;

/// The state of one media slot, on a broadcast or on a player.
///
/// [`PublishStatus`] and [`PlayerStatus`](crate::PlayerStatus) both use it, so
/// a UI reports both the same way. Two `Failed` states are equal only when they
/// hold the same error. This lets a watcher tell an update from a repeat.
#[derive(Debug, Clone, Default)]
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
    /// On a broadcast, the hardware-to-software encoder fallback retries, and
    /// moq-audio reopens a microphone when the device returns. On a player,
    /// video and audio both try again after a pause.
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
pub enum RenditionState {
    /// Advertised, but nobody is watching, so nothing is encoded.
    #[default]
    Idle,
    /// Somebody is watching, and this encoder is running.
    Encoding {
        /// The encoder backend, such as `openh264` or `vaapi`.
        encoder: String,
    },
    /// The encoder failed, and so did its fallback if there was one.
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
pub struct PublishStatus {
    /// The video slot.
    pub video: SlotState,
    /// The audio slot.
    pub audio: SlotState,
    /// The state of each rendition, video and audio alike, keyed by track name.
    pub renditions: BTreeMap<String, RenditionState>,
}

/// Which slot a report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Medium {
    Video,
    Audio,
}

/// The owner of each slot.
#[derive(Debug, Default)]
struct Owners {
    video: Owner,
    audio: Owner,
}

/// The task generation that owns a slot, with its rendition names.
#[derive(Debug, Default)]
struct Owner {
    generation: u64,
    names: Vec<String>,
}

impl Owners {
    fn get_mut(&mut self, medium: Medium) -> &mut Owner {
        match medium {
            Medium::Video => &mut self.video,
            Medium::Audio => &mut self.audio,
        }
    }
}

impl PublishStatus {
    fn slot(&mut self, medium: Medium) -> &mut SlotState {
        match medium {
            Medium::Video => &mut self.video,
            Medium::Audio => &mut self.audio,
        }
    }
}

/// The broadcast's status, written only by the task that owns each slot.
///
/// A replaced publish task can still be finishing when its replacement starts.
/// Every write goes through a [`Reporter`] that carries its task's generation.
/// The write is dropped unless that generation still owns the slot. The owners'
/// lock is held across the check and the write, so the two are atomic.
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusCell {
    owners: Arc<Mutex<Owners>>,
    watch: Watchable<PublishStatus>,
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

    /// Hands `medium` to the task of `generation` and returns its reporter.
    ///
    /// The renditions of the previous owner are removed.
    pub(crate) fn begin(&self, medium: Medium, generation: u64, names: &[String]) -> Reporter {
        let mut owners = self.owners.lock().expect("poisoned");
        let previous = std::mem::replace(
            owners.get_mut(medium),
            Owner {
                generation,
                names: names.to_vec(),
            },
        );
        let mut status = self.watch.get();
        for name in &previous.names {
            status.renditions.remove(name);
        }
        *status.slot(medium) = SlotState::Starting;
        for name in names {
            status.renditions.insert(name.clone(), RenditionState::Idle);
        }
        self.watch.set(status).ok();
        Reporter {
            cell: self.clone(),
            medium,
            generation,
        }
    }

    /// Turns `medium` off if the task of `generation` still owns it.
    pub(crate) fn clear(&self, medium: Medium, generation: u64) {
        let mut owners = self.owners.lock().expect("poisoned");
        let owner = owners.get_mut(medium);
        if owner.generation != generation {
            return;
        }
        let names = std::mem::take(&mut owner.names);
        let mut status = self.watch.get();
        for name in &names {
            status.renditions.remove(name);
        }
        *status.slot(medium) = SlotState::Off;
        self.watch.set(status).ok();
    }

    fn write(&self, medium: Medium, generation: u64, f: impl FnOnce(&mut PublishStatus)) {
        let mut owners = self.owners.lock().expect("poisoned");
        if owners.get_mut(medium).generation != generation {
            return;
        }
        let mut status = self.watch.get();
        f(&mut status);
        self.watch.set(status).ok();
    }
}

/// Writes one slot task's state into a [`StatusCell`].
#[derive(Debug, Clone)]
pub(crate) struct Reporter {
    cell: StatusCell,
    medium: Medium,
    generation: u64,
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

    /// A replaced task still finishing cannot overwrite its replacement's state.
    #[test]
    fn a_replaced_task_cannot_write() {
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
    fn a_new_task_replaces_the_old_renditions_only_for_its_medium() {
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
