use std::thread;

#[cfg(test)]
use hang::container::OrderedFrame;

#[cfg(test)]
use crate::av::EncodedFrame;

/// Spawn a named OS thread and panic if spawning fails.
pub(crate) fn spawn_thread<F, T>(name: impl ToString, f: F) -> thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let name_str = name.to_string();
    thread::Builder::new()
        .name(name_str.clone())
        .spawn(f)
        .unwrap_or_else(|_| panic!("failed to spawn thread: {}", name_str))
}

#[cfg(test)]
pub(crate) fn encoded_frames_to_ordered_frames(input: Vec<EncodedFrame>) -> Vec<OrderedFrame> {
    let mut out = Vec::with_capacity(input.len());
    let mut first = true;
    let mut group = 0;
    let mut index = 0;
    for frame in input {
        out.push(OrderedFrame {
            timestamp: frame.frame.timestamp,
            payload: frame.frame.payload,
            group,
            index,
        });
        if !first && frame.is_keyframe {
            group += 1;
            index = 0;
        } else {
            index += 1;
        }
        if first {
            first = false;
        }
    }
    out
}
