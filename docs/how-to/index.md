# Build an app

These guides each do one task in a Rust application, with the `irl` command
first where one exists. Each program on these pages compiles against the
current API.

- [Publish a camera, screen or file](publish.md): open a source and encode it
  into a ladder of renditions.
- [Watch a broadcast and render it](watch.md): play a broadcast's audio and
  draw its video with wgpu or egui.
- [Make a one-to-one call](call.md): ring a peer, answer, and hang up.
- [Run a room](rooms.md): join a gossip room and read every member's
  broadcasts.
- [Record a broadcast](record.md): write a broadcast to an MP4 or MKV file.
- [Tune quality and latency](tune.md): pick renditions, latency and
  adaptation.
- [Control access](control-access.md): decide which peers may connect, read
  and publish.
- [Use tokens](tokens.md): verify JWTs and rcan chains.
- [Publish through a relay](publish-through-relay.md): push broadcasts to a
  MoQ relay and read them from one.
- [Publish at your own path](custom-paths.md): use a path scheme of your own,
  and show a broadcast to chosen peers.
- [Send data without media](data.md): a control channel, latest-value state,
  and the node without the media crates.
