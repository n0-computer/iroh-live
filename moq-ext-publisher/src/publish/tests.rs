//! The publish side's own transitions.

use std::time::Duration;

use moq_ext_sources::SlotState;

use super::*;
use crate::audio_encoding;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Waits until the broadcast's audio slot reads `state`.
async fn audio_becomes(broadcast: &LocalBroadcast, state: SlotState) {
    let mut status = broadcast.status();
    tokio::time::timeout(TIMEOUT, status.wait_for(|status| status.audio == state))
        .await
        .unwrap_or_else(|_| panic!("the audio slot never became {state:?}"))
        .expect("the broadcast is alive");
}

/// A replacement waits for its predecessor's track names.
///
/// The test holds the audio track lock itself. The new publish stays at
/// `Starting` until the test releases it. A second broadcast without the lock
/// shows that the first had time to start.
#[tokio::test]
async fn a_replacement_waits_for_the_track_names() {
    let broadcast = LocalBroadcast::new();
    let control = LocalBroadcast::new();
    let held = broadcast.shared.audio_tracks.clone().lock_owned().await;
    for target in [&broadcast, &control] {
        target
            .set_audio(
                AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
                audio_encoding::voice(),
            )
            .expect("a valid encoding");
    }

    audio_becomes(&control, SlotState::Running).await;
    assert_eq!(
        broadcast.status().borrow().audio,
        SlotState::Starting,
        "the publish created its track while the name was held"
    );
    assert!(
        broadcast.stats().audio.is_none(),
        "the publish started encoding while the name was held"
    );

    drop(held);
    audio_becomes(&broadcast, SlotState::Running).await;
}

/// A second `set_audio` takes over the same track name and keeps publishing.
#[tokio::test]
async fn two_audio_sets_in_a_row_keep_publishing() {
    let broadcast = LocalBroadcast::new();
    let tone =
        || AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts");
    broadcast
        .set_audio(tone(), audio_encoding::voice())
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    broadcast
        .set_audio(tone(), audio_encoding::voice())
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    let frames = |broadcast: &LocalBroadcast| {
        broadcast
            .stats()
            .audio
            .map(|audio| audio.frames)
            .unwrap_or_default()
    };
    let before = frames(&broadcast);
    tokio::time::timeout(TIMEOUT, async {
        while frames(&broadcast) <= before + 5 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the replacement keeps encoding");
}

/// A cleared slot reports `Off` straight away, whatever its task was doing.
#[tokio::test]
async fn clearing_audio_turns_it_off() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
            audio_encoding::voice(),
        )
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    broadcast.clear_audio();
    assert_eq!(broadcast.status().borrow().audio, SlotState::Off);
    assert!(broadcast.stats().audio.is_none());
}

/// Closing ends every slot and the broadcast itself.
#[tokio::test]
async fn closing_finishes_the_broadcast() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
            audio_encoding::voice(),
        )
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    let consumer = broadcast.consume();
    broadcast.close();
    tokio::time::timeout(TIMEOUT, broadcast.closed())
        .await
        .expect("the close finished");
    tokio::time::timeout(TIMEOUT, consumer.closed())
        .await
        .expect("subscribers saw the end");
    assert!(matches!(
        broadcast.set_audio(
            AudioSource::tone(1.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
            audio_encoding::voice()
        ),
        Err(Error::Closed { .. })
    ));
}

/// A pushed audio source ends its slot once every sender is dropped.
#[tokio::test]
async fn a_pushed_audio_source_ends_when_its_senders_go() {
    let format = moq_ext_sources::AudioFormat {
        sample_rate: 48_000,
        layout: moq_audio::Layout::Mono,
    };
    let (sender, source) = AudioSource::push(format);
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(source, audio_encoding::voice())
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    let silence = bytes::Bytes::from(vec![0u8; 960 * 4]);
    sender
        .push(moq_audio::Frame::new(
            silence,
            moq_net::Timestamp::from_micros(0).expect("0"),
        ))
        .expect("the source is open");
    drop(sender);
    audio_becomes(&broadcast, SlotState::Ended).await;
}

/// Publishes `source` as the one rendition `rendition`, watches it, and waits until it sent frames.
///
/// Returns the broadcast, the rendition's track name, and the subscription that watches it.
async fn watch_until_sent(
    source: VideoSource,
    rendition: crate::VideoRendition,
) -> (LocalBroadcast, String, impl Sized) {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(source, crate::VideoEncoding::single(rendition))
        .expect("a valid encoding");
    let name = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(name) = broadcast.stats().renditions.keys().next() {
                return name.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the rendition starts");
    let track = broadcast.consume().track(&name).expect("the track exists");
    let subscribing = track.subscribe(None);
    tokio::time::timeout(TIMEOUT, async {
        while sent(&broadcast, &name).frames == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the rendition counts its frames");
    (broadcast, name, subscribing)
}

/// Returns the catalog's counters of the track rendition `name` publishes.
fn sent(broadcast: &LocalBroadcast, name: &str) -> hang::stats::Rendition {
    let stats = broadcast.stats();
    stats
        .sent
        .get(&stats.renditions[name].track)
        .cloned()
        .unwrap_or_default()
}

/// A watched rendition's frame counter comes from the catalog's counters of its track.
#[tokio::test]
async fn a_watched_rendition_counts_its_frames() {
    let source = VideoSource::test_pattern(
        moq_video::Size::new(320, 180),
        moq_video::FrameRate::new(30, 1).expect("a valid rate"),
    )
    .expect("the pattern thread starts");
    let (broadcast, name, _subscribing) =
        watch_until_sent(source, crate::VideoRendition::new("180p")).await;
    assert!(sent(&broadcast, &name).bytes > 0);
}

/// A rendition smaller than its source encodes frames the encoder scales across the SD boundary.
///
/// The encoder's thread scales the unlabelled 720p frames to 180p and labels
/// them with the space 720p implies, which differs from the one 180p implies.
/// The rendition takes that label instead of failing on a color mismatch.
#[tokio::test]
async fn a_rendition_smaller_than_its_source_encodes_scaled_frames() {
    let size = moq_video::Size::new(1280, 720);
    let rate = moq_video::FrameRate::new(30, 1).expect("a valid rate");
    let (sender, source) = VideoSource::push(moq_ext_sources::VideoFormat { size, rate });
    let pushing = tokio::spawn(async move {
        let clock = moq_mux::Clock::new();
        let mut tick = tokio::time::interval(Duration::from_millis(33));
        loop {
            tick.tick().await;
            let pixels = vec![0x80; moq_video::I420::len(size).expect("a valid size")];
            let picture = moq_video::I420::new(size, pixels).expect("a whole picture");
            assert_eq!(picture.color(), None, "the frame names no space");
            let frame = moq_video::Frame::new(moq_video::Surface::I420(picture), clock.now());
            if sender.push(frame).is_err() {
                return;
            }
        }
    });
    let mut rendition = crate::VideoRendition::new("180p");
    rendition.size = Some(moq_video::Size::new(320, 180));
    let (broadcast, name, _subscribing) = watch_until_sent(source, rendition).await;
    assert_eq!(
        broadcast.stats().renditions[&name].size,
        Some(moq_video::Size::new(320, 180))
    );
    assert!(
        matches!(
            broadcast.status().borrow().renditions.get(&name),
            Some(crate::RenditionState::Encoding { .. })
        ),
        "the rendition encodes"
    );
    pushing.abort();
}

/// A source whose frames change size mid-stream, as a reopened capture's do, keeps encoding at the track's size.
#[tokio::test]
async fn a_source_that_changes_size_keeps_encoding() {
    let opened = moq_video::Size::new(320, 180);
    let resized = moq_video::Size::new(400, 300);
    let rate = moq_video::FrameRate::new(30, 1).expect("a valid rate");
    let (sender, source) = VideoSource::push(moq_ext_sources::VideoFormat { size: opened, rate });
    let (resize, mut resizing) = tokio::sync::watch::channel(false);
    let pushing = tokio::spawn(async move {
        let clock = moq_mux::Clock::new();
        let mut tick = tokio::time::interval(Duration::from_millis(33));
        loop {
            tick.tick().await;
            let size = if *resizing.borrow_and_update() {
                resized
            } else {
                opened
            };
            let pixels = vec![0x80; moq_video::I420::len(size).expect("a valid size")];
            let picture = moq_video::I420::new(size, pixels).expect("a whole picture");
            let frame = moq_video::Frame::new(moq_video::Surface::I420(picture), clock.now());
            if sender.push(frame).is_err() {
                return;
            }
        }
    });
    let (broadcast, name, _subscribing) =
        watch_until_sent(source, crate::VideoRendition::new("180p")).await;
    resize.send_replace(true);
    let before = sent(&broadcast, &name).frames;
    tokio::time::timeout(TIMEOUT, async {
        while sent(&broadcast, &name).frames < before + 10 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the rendition goes on after the resize");
    assert_eq!(broadcast.stats().renditions[&name].size, Some(opened));
    assert!(
        matches!(
            broadcast.status().borrow().renditions.get(&name),
            Some(crate::RenditionState::Encoding { .. })
        ),
        "the rendition encodes"
    );
    pushing.abort();
}

/// A value track carries its newest value to a reader, and refuses a taken name and a closed broadcast.
#[tokio::test]
async fn a_value_track_reaches_a_reader_of_the_broadcast() {
    let broadcast = LocalBroadcast::new();
    let writer = broadcast.value_track::<u32>("control").expect("a new name");
    // Two scalar updates with no pause, so most likely in one millisecond.
    writer.update(&30).expect("write");
    writer.update(&60).expect("write");

    let track = broadcast.consume().track("control").expect("the track");
    let mut reader = moq_json::snapshot::Consumer::<u32>::new(track, Default::default());
    let newest = tokio::time::timeout(TIMEOUT, reader.latest())
        .await
        .expect("a value in time")
        .expect("a readable track")
        .expect("the track is open");
    assert_eq!(newest.value, 60, "the newest value, not an older one");

    assert!(
        broadcast.value_track::<u32>("control").is_err(),
        "a second track took a taken name"
    );
    broadcast.close();
    assert!(
        matches!(
            broadcast.value_track::<u32>("other"),
            Err(Error::Closed { .. })
        ),
        "a closed broadcast took a track"
    );
}

/// The broadcast hands back the raw sources it encodes, and nothing once cleared.
#[tokio::test]
async fn the_broadcast_hands_back_its_sources() {
    let broadcast = LocalBroadcast::new();
    assert!(broadcast.video_source().is_none());
    assert!(broadcast.audio_source().is_none());

    let video = VideoSource::test_pattern(
        moq_video::Size::new(320, 180),
        moq_video::FrameRate::new(30, 1).expect("a valid rate"),
    )
    .expect("the pattern thread starts");
    broadcast
        .set_video(
            video,
            crate::VideoEncoding::single(crate::VideoRendition::new("180p")),
        )
        .expect("a valid encoding");
    let audio = AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts");
    broadcast
        .set_audio(audio, audio_encoding::voice())
        .expect("a valid encoding");
    let kept = broadcast.video_source().expect("the video source");
    assert_eq!(kept.format().size, moq_video::Size::new(320, 180));
    assert_eq!(
        broadcast.audio_source().expect("the audio source").label(),
        "tone"
    );

    broadcast.clear_video();
    broadcast.clear_audio();
    assert!(broadcast.video_source().is_none());
    assert!(broadcast.audio_source().is_none());

    // A pre-encoded stream has no raw source to hand back.
    broadcast
        .set_encoded_video(moq_ext_sources::EncodedVideo::AnnexB(Box::pin(
            n0_future::stream::empty(),
        )))
        .expect("open");
    assert!(broadcast.video_source().is_none());
}

/// The spacing of the frames a keyframe test pushes once its renditions run, a little over 30 fps.
const STEP: u64 = 34_000;

/// Returns a grey 320x180 frame stamped at `micros`.
fn grey(micros: u64) -> moq_video::Frame {
    let size = moq_video::Size::new(320, 180);
    let pixels = vec![0x80; moq_video::I420::len(size).expect("a valid size")];
    let picture = moq_video::I420::new(size, pixels).expect("a whole picture");
    let timestamp = moq_net::Timestamp::from_micros(micros).expect("in range");
    moq_video::Frame::new(moq_video::Surface::I420(picture), timestamp)
}

/// Returns the catalog's counters of every rendition in `names`.
fn sent_all(broadcast: &LocalBroadcast, names: &[String]) -> Vec<hang::stats::Rendition> {
    names.iter().map(|name| sent(broadcast, name)).collect()
}

/// Waits until `grew` holds for the counters of every rendition in `names` against `before`.
async fn until_every(
    broadcast: &LocalBroadcast,
    names: &[String],
    before: &[hang::stats::Rendition],
    grew: impl Fn(&hang::stats::Rendition, &hang::stats::Rendition) -> bool,
) {
    tokio::time::timeout(TIMEOUT, async {
        while !sent_all(broadcast, names)
            .iter()
            .zip(before)
            .all(|(now, before)| grew(now, before))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the renditions published in time");
}

/// Encodes the 320x180 source behind `sender` into `renditions`, and waits until every one publishes.
///
/// Then it asks for a keyframe and pushes one more frame, later than any
/// rendition's frame interval, and waits until every rendition published a
/// keyframe for it. Since only that frame carries the request, no frame of
/// the start is still on its way after that. Returns the broadcast, the
/// timestamp of that frame in microseconds, and the subscriptions that watch
/// the renditions.
async fn settled_ladder(
    sender: &moq_ext_sources::FrameSender<moq_video::Frame>,
    source: VideoSource,
    renditions: Vec<crate::VideoRendition>,
) -> (LocalBroadcast, u64, Vec<impl Sized>) {
    let clock = moq_mux::Clock::new();
    let names: Vec<String> = renditions
        .iter()
        .map(|rendition| rendition.name.clone())
        .collect();
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(source, crate::VideoEncoding::ladder(renditions))
        .expect("a valid encoding");
    let push_now = || {
        let micros = super::micros(clock.now()) as u64;
        sender.push(grey(micros)).expect("the source is open");
        micros
    };
    let mut last = 0;
    tokio::time::timeout(TIMEOUT, async {
        while broadcast.stats().renditions.len() < names.len() {
            last = push_now();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the renditions start");
    let mut subscriptions = Vec::new();
    for name in &names {
        let track = &broadcast.stats().renditions[name].track;
        let track = broadcast.consume().track(track).expect("the track exists");
        subscriptions.push(track.subscribe(None));
    }
    tokio::time::timeout(TIMEOUT, async {
        while sent_all(&broadcast, &names)
            .iter()
            .any(|sent| sent.frames == 0)
        {
            last = push_now();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("every rendition encodes");

    let before = sent_all(&broadcast, &names);
    sender.request_keyframe();
    let marker = last + 100_000;
    sender.push(grey(marker)).expect("the source is open");
    until_every(&broadcast, &names, &before, |now, before| {
        now.keyframes > before.keyframes
    })
    .await;
    (broadcast, marker, subscriptions)
}

/// Returns a rendition that keys every ten seconds on its own, so every other keyframe is a requested one.
fn rarely_keyed(name: &str) -> crate::VideoRendition {
    let mut rendition = crate::VideoRendition::new(name);
    rendition.config.join = Duration::from_secs(10);
    rendition
}

/// A producer's keyframe request makes the next frame it pushes a keyframe on every rendition.
///
/// Each frame is pushed once both renditions published the previous one, so
/// none is skipped, and the keyframe spacing of each rendition is the spacing
/// of the two requested frames.
#[tokio::test]
async fn a_keyframe_request_keys_the_next_frame_on_every_rendition() {
    let size = moq_video::Size::new(320, 180);
    let rate = moq_video::FrameRate::new(30, 1).expect("a valid rate");
    let (sender, source) = VideoSource::push(moq_ext_sources::VideoFormat { size, rate });
    let renditions = vec![
        rarely_keyed("full"),
        rarely_keyed("small").with_size(moq_video::Size::new(160, 90)),
    ];
    let names = ["full".to_string(), "small".to_string()];
    let (broadcast, mut last, _subscriptions) = settled_ladder(&sender, source, renditions).await;
    let start = sent_all(&broadcast, &names);

    let requested = [3, 9];
    for index in 0..12 {
        let before = sent_all(&broadcast, &names);
        if requested.contains(&index) {
            sender.request_keyframe();
        }
        last += STEP;
        sender.push(grey(last)).expect("the source is open");
        until_every(&broadcast, &names, &before, |now, before| {
            now.frames > before.frames
        })
        .await;
        if requested.contains(&index) {
            until_every(&broadcast, &names, &before, |now, before| {
                now.keyframes > before.keyframes
            })
            .await;
        }
    }

    let stats = broadcast.stats();
    for (name, start) in names.iter().zip(start) {
        assert_eq!(
            sent(&broadcast, name).keyframes,
            start.keyframes + 2,
            "{name} keyed on the requested frames only"
        );
        assert_eq!(
            stats.renditions[name].keyframe_interval,
            Some(Duration::from_micros(6 * STEP)),
            "{name} keyed on the two requested frames"
        );
    }
}

/// A request before a frame that a rendition's frame-rate cap drops keys the next frame that rendition encodes.
///
/// The `half` rendition keeps a frame at most every 1/15 s, so of the frames
/// [`STEP`] apart it drops the first after the start and keeps the second.
/// The `full` rendition encodes every frame and shows when one was read.
#[tokio::test]
async fn a_keyframe_request_survives_a_frame_the_rate_cap_drops() {
    let size = moq_video::Size::new(320, 180);
    let rate = moq_video::FrameRate::new(30, 1).expect("a valid rate");
    let (sender, source) = VideoSource::push(moq_ext_sources::VideoFormat { size, rate });
    let capped = moq_video::FrameRate::new(15, 1).expect("a valid rate");
    let renditions = vec![rarely_keyed("full"), rarely_keyed("half").with_rate(capped)];
    let names = ["full".to_string(), "half".to_string()];
    let (broadcast, marker, _subscriptions) = settled_ladder(&sender, source, renditions).await;
    let [full, half] = [&names[0], &names[1]];
    let start = sent_all(&broadcast, &names);

    // Requested, and dropped by the half rendition.
    sender.request_keyframe();
    sender
        .push(grey(marker + STEP))
        .expect("the source is open");
    until_every(&broadcast, &names[..1], &start[..1], |now, before| {
        now.keyframes > before.keyframes
    })
    .await;
    // Not requested, and the first frame the half rendition encodes after the request.
    let before = sent_all(&broadcast, &names);
    sender
        .push(grey(marker + 2 * STEP))
        .expect("the source is open");
    until_every(&broadcast, &names, &before, |now, before| {
        now.frames > before.frames
    })
    .await;
    until_every(&broadcast, &names[1..], &start[1..], |now, before| {
        now.keyframes > before.keyframes
    })
    .await;

    assert_eq!(
        sent(&broadcast, half).frames,
        start[1].frames + 1,
        "the half rendition dropped the requested frame"
    );
    assert_eq!(
        sent(&broadcast, full).keyframes,
        start[0].keyframes + 1,
        "the full rendition keyed the requested frame only"
    );
    let stats = broadcast.stats();
    assert_eq!(
        stats.renditions[half].keyframe_interval,
        Some(Duration::from_micros(2 * STEP)),
        "the half rendition keyed the frame after the dropped one"
    );
    assert_eq!(
        stats.renditions[full].keyframe_interval,
        Some(Duration::from_micros(STEP)),
        "the full rendition keyed the requested frame"
    );
}

/// Returns the MP4 file `data` with its video track turned into a `free` box.
///
/// The box keeps its size, so every sample offset stays valid.
#[cfg(feature = "file")]
fn without_video(mut data: Vec<u8>) -> Vec<u8> {
    let size = |data: &[u8], at: usize| {
        u32::from_be_bytes(data[at..at + 4].try_into().expect("four bytes")) as usize
    };
    let mut moov = 0;
    while &data[moov + 4..moov + 8] != b"moov" {
        moov += size(&data, moov);
    }
    let mut child = moov + 8;
    while child < moov + size(&data, moov) {
        let trak = &data[child..child + size(&data, child)];
        // `hdlr`, then a version, flags and a predefined word before the handler type.
        let video = trak
            .windows(16)
            .any(|bytes| &bytes[..4] == b"hdlr" && &bytes[12..] == b"vide");
        if &trak[4..8] == b"trak" && video {
            data[child + 4..child + 8].copy_from_slice(b"free");
        }
        child += size(&data, child);
    }
    data
}

/// A file's H.264 and AAC publish as they are, B-frames included, and the slots end with the file.
///
/// A file without video publishes its audio alone.
#[cfg(feature = "file")]
#[tokio::test(start_paused = true)]
async fn a_file_passes_through() {
    let clips = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../moq-ext-sources/src/source/test_data");
    let clip = std::fs::read(clips.join("clip.mp4")).expect("the clip reads");
    let audio_only = std::env::temp_dir().join(format!(
        "moq-ext-publisher-audio-only-{}.mp4",
        std::process::id()
    ));
    std::fs::write(&audio_only, without_video(clip)).expect("the temp dir is writable");
    for path in [
        clips.join("clip.mp4"),
        clips.join("bframes.mp4"),
        audio_only.clone(),
    ] {
        let name = path.display();
        let file = moq_ext_sources::MediaFile::open(&path)
            .await
            .expect("the clip opens");
        let has_video = file.info().video.is_some();
        let has_audio = file.info().audio.is_some();
        if path == audio_only {
            assert!(!has_video && has_audio, "{name} still has its video");
        }
        let broadcast = LocalBroadcast::new();
        broadcast.set_encoded_file(&file).expect("H.264 or AAC");
        // The catalog lists the renditions while they play, described by the file's records.
        let catalog = || {
            broadcast
                .shared
                .catalog
                .lock()
                .expect("poisoned")
                .snapshot()
        };
        tokio::time::timeout(TIMEOUT, async {
            while (has_video && catalog().video.renditions.is_empty())
                || (has_audio && catalog().audio.renditions.is_empty())
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{name} published no rendition"));
        let ended = |has: bool, state: &SlotState| {
            if has {
                *state == SlotState::Ended
            } else {
                *state == SlotState::Off
            }
        };
        let mut status = broadcast.status();
        tokio::time::timeout(
            TIMEOUT,
            status.wait_for(|status| {
                ended(has_video, &status.video) && ended(has_audio, &status.audio)
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("{name} did not end: {:?}", broadcast.status().borrow()))
        .expect("the broadcast is alive");
        if has_audio {
            let frames = broadcast.stats().audio.expect("audio stats").frames;
            assert!(frames > 10, "{name}: {frames} audio frames");
        }
    }
    std::fs::remove_file(&audio_only).expect("the file was written");
}

/// Two pre-encoded streams that agree on their frames' instants keep their offset on the broadcast clock.
///
/// The audio's first frame arrives later than the video's, and presents 64 ms
/// after it, as the audio of a file with an encoder delay does.
#[tokio::test]
async fn pre_encoded_streams_stay_aligned() {
    let clock = moq_mux::Clock::new();
    let start = std::time::Instant::now();
    let frame = |millis: u64| {
        moq_ext_sources::EncodedFrame::new(
            bytes::Bytes::new(),
            moq_net::Timestamp::from_micros(millis * 1000).unwrap(),
            start + Duration::from_millis(millis),
        )
    };
    let (mut video, mut audio) = (None, None);
    let video_at = super::wall_map(&mut video, &clock, &frame(0));
    std::thread::sleep(Duration::from_millis(30));
    let audio_at = super::wall_map(&mut audio, &clock, &frame(64));
    let apart = super::micros(audio_at) as i128 - super::micros(video_at) as i128;
    assert!((apart - 64_000).abs() < 1_000, "{apart} us apart");
}
