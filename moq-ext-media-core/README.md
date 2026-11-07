# moq-ext-media-core

The few types [`moq-ext-sources`](../moq-ext-sources),
[`moq-ext-player`](../moq-ext-player) and
[`moq-ext-publisher`](../moq-ext-publisher) share, so an error passes from a
source to a broadcast unchanged, and a UI handles a broadcast and a player the
same way.

Use it when you write code that works with both sides and should not depend on
either. Most applications never name it: the other crates re-export what they
need.

- `Error`: the one error all three crates report.
- `Closed`: what pushing to a source that has gone returns.
- `SlotState`, `ByPtr`, `send_if_changed`: the state of a rendition slot, and
  helpers to publish it through a watch channel only when it changed.
- `stats`: rate counters, so both sides count the same way.

```rust
use moq_ext_media_core::{Error, SlotState};
```

No media or transport dependency.

## Features

None.
