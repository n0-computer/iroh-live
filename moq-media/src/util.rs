use std::thread;

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
pub(crate) fn encoded_frames_to_media_packets(
    input: Vec<crate::format::EncodedFrame>,
) -> Vec<crate::format::MediaPacket> {
    input
        .into_iter()
        .map(|frame| crate::format::MediaPacket {
            timestamp: frame.timestamp,
            payload: frame.payload.into(),
            is_keyframe: frame.is_keyframe,
        })
        .collect()
}
