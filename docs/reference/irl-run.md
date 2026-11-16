# The `irl run` session file

`irl run` runs a session from a TOML file: one endpoint that publishes
several broadcasts, subscribes to others, and records any of them. Separate
`irl publish` and `irl watch` processes would each bind their own endpoint,
and so print tickets with different endpoint ids.

```sh
irl run session.toml
```

The session opens no window. A `[[recv]]` block plays audio and can record.

```toml
# Optional. Keeps the endpoint id, and so the tickets, across runs. The key is
# stored at <config dir>/iroh-live/secret_keys/<name>.key.
secret_key_name = "studio"

[[send]]
name = "camera"              # broadcast name
video = "cam"                # the other keys are `irl publish` flags,
audio = "mic"                # with the same defaults
codec = "h264"
encoder = "auto"
renditions = ["low:320x180", "720p"]
bitrate = 3_000_000
width = 1280
height = 720
fps = 30
no_cursor = false
audio_codec = "opus"
audio_bitrate = 96_000

[[send]]
name = "screen"
video = "screen"
audio = "none"

[[recv]]
name = "friend"
ticket = "ilv1:..."         # as `irl publish` printed it
audio_output = "default"     # or "none"
record = "friend.mp4"        # optional, the extension picks the container
rendition = "low"            # optional, the rendition to record
```

A `[[send]]` block needs only `name`, and a `[[recv]]` block needs `name` and
`ticket`. `[[send]]` does not take a `file:` video source. An unknown key is an
error.

A block that fails to start is reported, and the rest of the session runs.
Ctrl+C ends the session and finishes every recording before the endpoint
closes.

Without `secret_key_name`, the session reads its key from `IROH_SECRET`, as
the other commands do. The [CLI reference](cli.md#flags) describes the keys
of a `[[send]]` block, which are the flags of `irl publish`.
