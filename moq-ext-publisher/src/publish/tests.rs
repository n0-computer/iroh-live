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

/// A watched rendition's frame counter comes from the catalog's counters of its track.
#[tokio::test]
async fn a_watched_rendition_counts_its_frames() {
    let source = VideoSource::test_pattern(
        moq_video::Size::new(320, 180),
        moq_video::FrameRate::new(30, 1).expect("a valid rate"),
    )
    .expect("the pattern thread starts");
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            crate::VideoEncoding::single(crate::VideoRendition::new("180p")),
        )
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
    // The counters are the catalog's, under the track the rendition names.
    let sent = || {
        let stats = broadcast.stats();
        stats
            .sent
            .get(&stats.renditions[&name].track)
            .cloned()
            .unwrap_or_default()
    };
    tokio::time::timeout(TIMEOUT, async {
        while sent().frames == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the rendition counts its frames");
    assert!(sent().bytes > 0);
}
