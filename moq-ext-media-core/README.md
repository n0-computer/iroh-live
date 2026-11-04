# moq-ext-media-core

The few types [`moq-ext-player`](../moq-ext-player) and
[`moq-ext-publisher`](../moq-ext-publisher) share, so a UI handles a broadcast
and a player the same way.

Use it when you write code that works with both sides and should not depend on
either. Most applications never name it: the player and the publisher re-export
what they need.

- `Error`: the one error both crates report.
- `SlotState`, `ByPtr`, `send_if_changed`: the state of a rendition slot, and
  helpers to publish it through a watch channel only when it changed.
- `stats`: rate counters, so both sides count the same way.
- `Closed`: re-exported from `moq-ext-sources`.

```rust
use moq_ext_media_core::{Error, SlotState};
```

No transport dependency.

## Features

None.
