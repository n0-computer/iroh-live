use std::task::Poll;

use moq_json::{Timed, snapshot};
use moq_net::{Timescale, Timestamp, time::Instant, track};

use crate::ValueWriter;

/// Creates a writer on a millisecond track, and a reader that joined before any write.
fn writer() -> (ValueWriter<u64>, snapshot::Consumer<u64>) {
    let track = moq_net::broadcast::Info::new()
        .produce()
        .create_track(
            "value",
            track::Info::default().with_timescale(Timescale::MILLI),
        )
        .expect("a new track");
    let reader = snapshot::Consumer::new(track.consume(), Default::default());
    (ValueWriter::new(track, Default::default()), reader)
}

/// Reads every value the reader has without waiting.
fn drain(reader: &mut snapshot::Consumer<u64>) -> Vec<Timed<u64>> {
    let waiter = kio::Waiter::noop();
    let mut read = Vec::new();
    while let Poll::Ready(Ok(Some(value))) = reader.poll_next(&waiter) {
        read.push(value);
    }
    read
}

/// Returns the newest stamp the writer recorded.
fn last(writer: &ValueWriter<u64>) -> Timestamp {
    writer.lock().last.expect("a stamp")
}

/// Returns `at` in whole milliseconds.
fn millis(at: Timestamp) -> u128 {
    at.as_millis().expect("a timed stamp")
}

/// A burst of changed scalar updates all succeed, most of them in a millisecond another took.
#[test]
fn a_burst_of_scalar_updates_succeeds_in_order() {
    let (writer, mut reader) = writer();
    for value in 0..100 {
        writer.update(&value).expect("a stamp past the last");
    }

    let read = drain(&mut reader);
    assert_eq!(read.last().map(|value| value.value), Some(99));
    for pair in read.windows(2) {
        assert!(pair[0].value < pair[1].value, "values out of order");
        assert!(pair[0].at < pair[1].at, "stamps not increasing");
    }
    let now = millis(Timestamp::from(Instant::now()));
    assert!(
        millis(last(&writer)) <= now + 100,
        "the stamps ran more than one tick per update ahead of the clock"
    );
}

/// Two updates in one millisecond both succeed, and the second is stamped after the first.
#[test]
fn a_scalar_updated_twice_in_one_millisecond_succeeds() {
    let (writer, _reader) = writer();
    writer.update(&1).expect("the first update");
    let first = last(&writer);
    writer.update(&2).expect("the second update");
    assert!(last(&writer) > first);
}

/// An unchanged value writes nothing and leaves the stamp where it was.
#[test]
fn an_unchanged_value_does_not_advance_the_stamp() {
    let (writer, _reader) = writer();
    writer.update(&7).expect("write");
    let stamp = last(&writer);
    assert_eq!(writer.update(&7).expect("no-op"), None);
    assert_eq!(last(&writer), stamp);
}

/// An update after an explicit time ahead of the clock is stamped after that time.
#[test]
fn update_after_update_at_ahead_of_the_clock_stamps_later() {
    let (writer, mut reader) = writer();
    let now = millis(Timestamp::from(Instant::now())) as u64;
    let ahead = Timestamp::from_millis(now + 1_000).expect("in range");
    writer.update_at(&1, ahead).expect("write ahead");
    writer.update(&2).expect("write after");
    assert!(last(&writer) > ahead);

    let read = drain(&mut reader);
    let newest = read.last().expect("a value");
    assert_eq!(newest.value, 2);
    assert!(newest.at > ahead);
}

/// The clock stepping back is the same as a tie: the stamp moves past the last one.
#[test]
fn a_clock_behind_the_last_stamp_is_moved_past_it() {
    let (writer, _reader) = writer();
    writer.update(&1).expect("write");
    let stamp = last(&writer);
    let shared = writer.lock();
    let behind = Timestamp::from_millis(0).expect("in range");
    let next = shared.next(behind).expect("a timed track");
    assert_eq!(millis(next), millis(stamp) + 1);
}

/// An explicit time before the previous group fails, and the next update recovers.
#[test]
fn update_at_before_the_previous_group_fails() {
    let (writer, mut reader) = writer();
    writer.update(&1).expect("write");
    let before = Timestamp::from_millis(0).expect("in range");
    let refused = writer.update_at(&2, before);
    assert!(
        matches!(
            refused,
            Err(moq_json::Error::Net(moq_net::Error::TimestampMismatch))
        ),
        "an earlier start was taken: {refused:?}"
    );
    writer.update(&3).expect("the next update");
    assert_eq!(drain(&mut reader).last().map(|value| value.value), Some(3));
}

/// Clones on two threads update the same track without a refused stamp.
#[test]
fn clones_on_two_threads_never_collide() {
    let (writer, _reader) = writer();
    std::thread::scope(|scope| {
        for parity in 0..2 {
            let writer = writer.clone();
            scope.spawn(move || {
                // Values of different parity never match, so every write starts a group.
                for step in 0..1_000 {
                    writer
                        .update(&(step * 2 + parity))
                        .expect("a stamp past the last");
                }
            });
        }
    });
}

/// `mutate` stamps like `update`, so two edits in one millisecond both succeed.
#[test]
fn mutate_twice_in_one_millisecond_succeeds() {
    let (writer, mut reader) = writer();
    writer.mutate(|value| *value += 1).expect("the first edit");
    writer.mutate(|value| *value += 1).expect("the second edit");
    assert_eq!(drain(&mut reader).last().map(|value| value.value), Some(2));
}

/// Dropping the last clone ends the track for a reader, and dropping one of two does not.
#[test]
fn dropping_the_last_clone_finishes_the_track() {
    let (writer, mut reader) = writer();
    let clone = writer.clone();
    writer.update(&1).expect("write");
    assert_eq!(drain(&mut reader).len(), 1);

    drop(writer);
    let waiter = kio::Waiter::noop();
    assert!(
        reader.poll_latest(&waiter).is_pending(),
        "a remaining clone keeps the track open"
    );

    drop(clone);
    assert!(
        matches!(reader.poll_latest(&waiter), Poll::Ready(Ok(None))),
        "the track did not end"
    );
}

/// After `finish`, a clone fails to update.
#[test]
fn a_clone_fails_after_finish() {
    let (writer, _reader) = writer();
    let clone = writer.clone();
    writer.update(&1).expect("write");
    writer.finish().expect("finish");
    let refused = clone.update(&2);
    assert!(
        matches!(refused, Err(moq_json::Error::Net(moq_net::Error::Closed))),
        "a finished track took an update: {refused:?}"
    );
}
