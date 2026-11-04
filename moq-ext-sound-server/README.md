# moq-ext-sound-server

Turns on moq-audio's PipeWire and PulseAudio backends on Linux, and does
nothing elsewhere. The crate has no code.

moq-audio's PulseAudio code does not compile off Linux, and a crate feature
cannot be target-specific. This crate carries the target table instead: a
dependent crate's `sound-server` feature pulls it in, and `--all-features`
then works on every platform.

Use it the way [`moq-ext-sources`](../moq-ext-sources) does:

```toml
[dependencies]
moq-ext-sound-server = { version = "0.1", optional = true }

[features]
sound-server = ["dep:moq-ext-sound-server"]
```

## Features

None.
