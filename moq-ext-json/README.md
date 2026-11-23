# moq-ext-json

A writer for latest-value JSON tracks that stamps each update itself.

A [`moq-json`](https://github.com/moq-dev/moq) snapshot track on a timed track
needs a timestamp on every update, and a group must start later than the group
before it. `ValueWriter` takes the stamp from the moq clock and moves it one
millisecond past the previous stamp when the clock has not advanced, so two
updates in the same millisecond both succeed. Clones share the track and the
stamps, and the last clone to drop finishes the track.

```rust
use moq_ext_json::ValueWriter;
use moq_net::{Timescale, track};

let broadcast = moq_net::broadcast::Info::new().produce();
let track = broadcast
    .create_track("latency", track::Info::default().with_timescale(Timescale::MILLI))
    .ok_or("the track exists")?;
let latency = ValueWriter::<u64>::new(track, Default::default());
latency.update(&150)?;
latency.update(&160)?;
```

`iroh_live_node::Node::publish_value` and
`moq_ext_publisher::LocalBroadcast::value_track` return a `ValueWriter`, and
the `iroh-live` facade re-exports it. Readers use `moq_json::snapshot::Consumer`
as before.

## Features

None.
