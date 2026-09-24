//! The publish side's own transitions.

use std::time::Duration;

use n0_watcher::Watcher as _;

use super::*;
use crate::{AudioEncoding, audio};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Waits until the broadcast's audio slot reads `state`.
async fn audio_becomes(broadcast: &LocalBroadcast, state: SlotState) {
    let mut status = broadcast.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get().audio != state {
            status.updated().await.expect("the broadcast is alive");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the audio slot never became {state:?}"));
}

/// R07: a replacement must not create a track whose name its predecessor still
/// holds. Driven through the slot lock itself: something holding the audio
/// track names keeps the new publish at `Starting`, and letting go lets it run.
#[tokio::test]
async fn a_replacement_waits_for_the_track_names() {
    let broadcast = LocalBroadcast::new();
    let held = broadcast.shared.audio_tracks.clone().lock_owned().await;
    broadcast
        .set_audio(
            AudioSource::tone(440.0, audio::Layout::Mono),
            AudioEncoding::voice(),
        )
        .expect("a valid encoding");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        broadcast.status().get().audio,
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

/// Two `set_audio` calls in a row name the same track; the second takes over
/// once the first has let it go, and keeps publishing.
#[tokio::test]
async fn two_audio_sets_in_a_row_keep_publishing() {
    let broadcast = LocalBroadcast::new();
    let tone = || AudioSource::tone(440.0, audio::Layout::Mono);
    broadcast
        .set_audio(tone(), AudioEncoding::voice())
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    broadcast
        .set_audio(tone(), AudioEncoding::voice())
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

/// S3: nothing used to fail if the microphone publication stopped building
/// the canceller its config asks for. The publication asks the output for one
/// when it starts, which a null output counts without any device behind it.
#[cfg(all(feature = "capture", feature = "aec"))]
#[tokio::test]
async fn a_microphone_publication_asks_for_its_canceller() {
    use crate::{AudioOutput, MicrophoneConfig};
    let output = AudioOutput::null();
    let config = MicrophoneConfig::default().with_echo_cancellation(&output);
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::microphone_unchecked(config),
            AudioEncoding::voice(),
        )
        .expect("a valid encoding");
    tokio::time::timeout(TIMEOUT, async {
        while output.cancellers_requested() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the publication never asked for an echo canceller");
}

/// A cleared slot reports `Off` straight away, whatever its task was doing.
#[tokio::test]
async fn clearing_audio_turns_it_off() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, audio::Layout::Mono),
            AudioEncoding::voice(),
        )
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    broadcast.clear_audio();
    assert_eq!(broadcast.status().get().audio, SlotState::Off);
    assert!(broadcast.stats().audio.is_none());
}

/// Closing ends every slot and the broadcast itself.
#[tokio::test]
async fn closing_finishes_the_broadcast() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, audio::Layout::Mono),
            AudioEncoding::voice(),
        )
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    let consumer = broadcast.as_moq().consume();
    broadcast.close();
    tokio::time::timeout(TIMEOUT, broadcast.closed())
        .await
        .expect("the close finished");
    tokio::time::timeout(TIMEOUT, consumer.closed())
        .await
        .expect("subscribers saw the end");
    assert!(matches!(
        broadcast.set_audio(
            AudioSource::tone(1.0, audio::Layout::Mono),
            AudioEncoding::voice()
        ),
        Err(Error::Closed { .. })
    ));
}

/// An audio track name a video rendition already has is refused at once.
#[tokio::test]
async fn an_audio_name_that_collides_with_video_is_refused() {
    let broadcast = LocalBroadcast::new();
    let source = VideoSource::test_pattern(
        crate::video::Size::new(64, 48),
        crate::video::Rate::new(30, 1).expect("valid"),
    );
    broadcast
        .set_video(source, VideoEncoding::single(VideoRendition::new("opus")))
        .expect("valid");
    let result = broadcast.set_audio(
        AudioSource::tone(440.0, audio::Layout::Mono),
        AudioEncoding::voice(),
    );
    assert!(matches!(result, Err(Error::InvalidConfig { .. })));
}
