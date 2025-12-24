/// Spawn a named OS thread and panic if spawning fails.
pub fn spawn_thread<F, T>(name: impl ToString, f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let name_str = name.to_string();
    std::thread::Builder::new()
        .name(name_str.clone())
        .spawn(f)
        .expect(&format!("failed to spawn thread: {}", name_str))
}
