//! The publish side's own transitions.

use std::time::Duration;

use moq_ext_sources::SlotState;
use moq_net::bandwidth::Rate;

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

/// A value track carries its newest value to a reader, and refuses a taken name and a closed broadcast.
#[tokio::test]
async fn a_value_track_reaches_a_reader_of_the_broadcast() {
    let broadcast = LocalBroadcast::new();
    let mut writer = broadcast.value_track::<u32>("control").expect("a new name");
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

/// Waits until rendition `name`'s target reads `target`, and returns how long that took.
async fn target_becomes(broadcast: &LocalBroadcast, name: &str, target: Option<Rate>) -> Duration {
    let start = tokio::time::Instant::now();
    tokio::time::timeout(TIMEOUT, async {
        while broadcast.stats().renditions[name].target != target {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the target stayed at {:?}, not {target:?}",
            broadcast.stats().renditions[name].target
        )
    });
    start.elapsed()
}

/// A watched rendition follows its grant: down at once, held without an estimate, up on a ramp.
#[tokio::test]
async fn a_rendition_follows_its_grant() {
    use moq_net::bandwidth::{Allocator, Producer};

    let estimate = Producer::new();
    let source = VideoSource::test_pattern(
        moq_video::Size::new(320, 180),
        moq_video::FrameRate::new(30, 1).expect("a valid rate"),
    )
    .expect("the pattern thread starts");
    let ceiling = Rate::from_kbps(1_000);
    let mut rendition = crate::VideoRendition::new("180p");
    rendition.config.bitrate = Some(ceiling);
    let broadcast = LocalBroadcast::new();
    broadcast.set_bandwidth(Allocator::new(estimate.consume()));
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
    let _subscribing = track.subscribe(None);

    target_becomes(&broadcast, &name, Some(ceiling)).await;
    assert_eq!(
        broadcast.stats().renditions[&name].grant,
        None,
        "no estimate yet"
    );

    let low = Rate::from_kbps(400);
    estimate.set(Some(low)).expect("the allocator reads it");
    let fell = target_becomes(&broadcast, &name, Some(low)).await;
    assert!(fell < Duration::from_secs(1), "the drop took {fell:?}");
    assert_eq!(broadcast.stats().renditions[&name].grant, Some(low));

    estimate.set(None).expect("the allocator reads it");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        broadcast.stats().renditions[&name].target,
        Some(low),
        "a lost estimate holds the bitrate"
    );

    estimate
        .set(Some(Rate::from_mbps(4)))
        .expect("the allocator reads it");
    let rose = target_becomes(&broadcast, &name, Some(ceiling)).await;
    // 25% a second from 400 kbit/s to 1 Mbit/s, never above the ceiling.
    assert!(rose > Duration::from_secs(2), "the rise took only {rose:?}");
}
