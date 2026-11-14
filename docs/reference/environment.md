# Environment variables

| Variable | Read by | Effect |
|---|---|---|
| `IROH_SECRET` | `irl`, `EndpointConfig::from_env()`, the examples and demos | The endpoint's secret key. Keeps the endpoint id, and so the tickets, across restarts |
| `IROH_LIVE_RELAY_DATA` | `iroh-live-relay` | The directory of the relay's secret key file |
| `RUST_LOG` | `irl`, `iroh-live-relay`, the examples | The log filter |

## `IROH_SECRET`

The value is a secret key as 64 hex characters. Make one with:

```sh
export IROH_SECRET=$(openssl rand -hex 32)
```

Without it, every start makes a new key and so a new endpoint id. A value that
is not a key fails with `node::Error::SecretKey`. The key is a secret: whoever has it
can act as this endpoint, and receive what peers send to it.

`irl run` takes `secret_key_name` in its session file instead, see
[The `irl run` session file](irl-run.md). An application can also keep its
key in a file with `iroh_live::load_or_create_secret_key(path)`.

## `IROH_LIVE_RELAY_DATA`

The relay keeps its key in `$IROH_LIVE_RELAY_DATA/iroh_secret_key`. Without
the variable, it uses `iroh-live-relay/` under the platform's data directory,
such as `~/.local/share/iroh-live-relay/` on Linux. The relay makes the key on
its first start.

## `RUST_LOG`

`RUST_LOG` takes a
[tracing filter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html):
a default level, and levels per crate or module, separated by commas. Without
it, `irl` and the examples log only errors, and the relay logs at `info`.

```sh
RUST_LOG=info irl publish
RUST_LOG=warn,iroh_live=debug irl watch <TICKET>
```

Useful filters:

| To see | Filter |
|---|---|
| What `irl` does: capture rates, encoders, renditions, connections | `info` |
| Sessions, admission and policy decisions | `iroh_live_node=debug` |
| Each link sample: round trip, loss, whether the path is relayed | `iroh_live_node::link=trace` |
| Why the player switched renditions, and playout delay changes | `moq_ext_player=trace,moq_ext_adaptive=debug` |
| Encoder start and failure | `moq_ext_publisher=debug` |
| Room membership | `iroh_live_rooms=debug` |
| iroh's connectivity: hole punching, iroh relays | `iroh=debug` |
| MoQ sessions and subscriptions | `moq_net=debug` |

`trace` is verbose. Prefer it for one module, not as the default level.

On Android, the demo sends the same logs to logcat, see
[Android](../how-to/android.md#debugging).
