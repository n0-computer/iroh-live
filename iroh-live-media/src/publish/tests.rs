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

/// A replacement waits for its predecessor's track names.
///
/// The test holds the audio track lock itself. The new publish stays at
/// `Starting` until the test releases it.
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

/// A second `set_audio` takes over the same track name and keeps publishing.
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

/// A microphone publication asks its echo reference output for a canceller.
///
/// A null output counts the request without any device behind it.
#[cfg(all(feature = "capture", feature = "aec"))]
#[tokio::test]
async fn a_microphone_publication_asks_for_its_canceller() {
    use crate::{AudioOutput, MicrophoneConfig};
    let output = AudioOutput::null();
    let config = MicrophoneConfig {
        echo_reference: Some(output.clone()),
        ..MicrophoneConfig::default()
    };
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
            AudioSource::tone(1.0, audio::Layout::Mono),
            AudioEncoding::voice()
        ),
        Err(Error::Closed { .. })
    ));
}

/// A pushed audio source ends its slot once every sender is dropped.
#[tokio::test]
async fn a_pushed_audio_source_ends_when_its_senders_go() {
    let format = crate::AudioFormat {
        sample_rate: 48_000,
        layout: audio::Layout::Mono,
    };
    let (sender, source) = AudioSource::push(format);
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(source, AudioEncoding::voice())
        .expect("valid");
    audio_becomes(&broadcast, SlotState::Running).await;
    let silence = bytes::Bytes::from(vec![0u8; 960 * 4]);
    sender
        .push(audio::Frame::new(
            silence,
            moq_net::Timestamp::from_micros(0).expect("0"),
        ))
        .expect("the source is open");
    drop(sender);
    audio_becomes(&broadcast, SlotState::Ended).await;
}
