# A/V sync basics

The older version of this page explained A/V sync through the now-removed
shared `PlayoutClock` design. The current receive side no longer works that
way, so the detailed walkthrough moved to the dedicated A/V sync section.

Start here instead:

- [A/V sync and playout](av-sync/README.md): the current receive-side design,
  including audio-master playout, `PlaybackPolicy`, `AudioPosition`, and
  `VideoSyncController`.
- [A/V sync tuning](av-sync/tuning.md): the public knobs and when to use them.

The codec-level background from the original page is still true in broad
strokes: video decode is burstier than audio decode, keyframes are the recovery
boundary, and live playback should avoid adding routine audio gaps. The part
that changed is how the receiver turns those facts into playout behavior.
