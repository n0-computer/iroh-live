# moq-ext-adaptive

Rendition choice and decoder handover for adaptive video, as pure state
machines. No I/O, no tokio, no media or transport types: every method that
depends on time takes an `Instant`, so tests drive them with a fake clock.

Use it when you build a player that switches between renditions. It is what
[`moq-ext-player`](../moq-ext-player) wires together.

- `Bound` and `choose`: pick the best rung of a ranked ladder (`Rung`) that the
  link (`NetworkSample`) carries, within the caller's `Constraints`, with
  hysteresis tuned by `Adaptation`. `RenditionMode` is automatic, pinned or
  off. `step_down` tells whether a switch moves down the ladder.
- `Backoffs`: renditions whose decoders failed, and when to try them again.
- `Switcher`: hands the picture from one decoder to the next without a blank
  frame.

```rust
use moq_ext_adaptive::{Adaptation, Bound};

let mut bound = Bound::new(Adaptation::default());
// Each pass: `bound.decide(&ranked, current, on_screen, &constraints, &sample, now)`
// returns the rendition to play.
```

See the crate docs for the full API and `src/bound.rs` for the tests.

## Features

None.
