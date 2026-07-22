# B2. Presentation timestamp through the encode path

> Campaign: upstream | Kind: base plan | Branch: up/base (lands alongside B1 in
> the base series) | PR target: base branch, then moq main | Read ../0-overview.md first.

Depends on: none (independent of B1; the base series can land B1 and B2 in either order)
Path: Both (needed for Path A and Path B)
Size: S-M (roughly 150 lines across about 7 files)

## Goal

Make moq's encode `Backend` carry the presentation timestamp into `encode` and
return it per packet, so a pipelined backend (V4L2 M2M, Android MediaCodec) can
drain a frame that entered the device queue several inputs ago and stamp its
bitstream honestly, rather than blocking a full device round trip per call or
mis-stamping frame N-k's output with frame N's time. Today the encode `Backend`
takes no timestamp (`encode/backend/mod.rs:40`) and the capture loop reads one
timestamp before the encode and stamps every returned packet with it
(`encode/producer.rs:386-392`), which is correct only for zero-frame-delay
backends. moq's shipped backends all guarantee zero delay, so this change is
additive in behavior for them (each echoes the current frame's timestamp), and it
makes encode symmetric with decode, which already carries a per-picture timestamp
in `Decoded { timestamp, frame }` (`decode/backend/mod.rs:53-62`).

## Evidence

- The dishonesty is precise and sourced: the capture loop computes `let ts =
  Timestamp::from_micros(clock.micros())?;` at `encode/producer.rs:386`, calls
  `encoder.encode(frame, force_keyframe)` at `:387`, and stamps every packet with
  that one `ts` at `producer.publish(packets, ts)?;` on `:392`. Correct only if the
  encoder has zero frame delay.
- Our V4L2 M2M encoder is a hardware queue where this is false by construction:
  `push_frame` sends `EncoderCmd::Encode { nv12, timestamp_us }` and returns
  immediately (`rusty-codecs/src/codec/v4l2/encoder.rs:267`), and bitstream for
  frame N surfaces during frame N+k; the PTS survives only because it rides the
  V4L2 buffer timestamp (`encoder.rs:718`, set at `:974-975`). Android MediaCodec is
  the same dequeue-based shape. Full analysis in `comparisons/traits-api.md`
  section 6 and `comparisons/moq-changes.md` section 2 point 1 (change 2).
- The decode side already got this right: `Decoded` carries its own `Timestamp`
  "so they survive decoder delay and frame reordering"
  (`decode/backend/mod.rs:53-62`). B2 makes encode match.
- Verified against `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`. B-frames are not
  the argument (nothing on either side emits them); the queue-based device model is.

## moq API consumed

None of the frozen-contract GPU types. B2 defines part of the frozen contract
itself: the new `encode::Backend::encode` signature and the `Packet` type. It is
independent of B1's `Native` vocabulary (the `frame: &Frame` argument is the same
private `crate::frame::Frame` moq passes today).

## Source to port

Nothing is ported wholesale. The change is a signature and a call-site rewrite in
moq's own code, informed by our V4L2 encoder's timestamp handling
(`rusty-codecs/src/codec/v4l2/encoder.rs:718, 974-975`) as the motivating example
of a backend that cannot honestly stamp at the call site. The V4L2 backend itself
is the v4l2-encode leaf, not B2; B2 only prepares the seam it needs.

## Target in moq

1. **The encode `Backend` trait** (`rs/moq-video/src/encode/backend/mod.rs:37-57`).
   Change `encode`:

   ```rust
   // was: fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>;   // :40
   fn encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe: bool)
       -> Result<Vec<Packet>, Error>;
   ```

   `finish` also returns `Vec<Packet>` for symmetry (`:43`), stamping any buffered
   tail with the timestamps the device reports. Add `use moq_net::Timestamp;` (the
   decode backend already imports it at `decode/backend/mod.rs:17`).

2. **The `Packet` type**, public because B4 may publish the trait and because the
   producer consumes it (place it in `encode/backend/mod.rs` or `encode/mod.rs`):

   ```rust
   /// One encoded access unit and the presentation time of the frame it belongs to.
   /// A pipelined backend returns the timestamp the device reported for the drained
   /// frame, not the timestamp of the frame just submitted.
   #[non_exhaustive]
   pub struct Packet {
       pub payload: Bytes,
       pub timestamp: Timestamp,   // moq_net::Timestamp
   }
   ```

3. **The five in-tree backends**, each echoing the current frame's timestamp so
   behavior is unchanged (`rs/moq-video/src/encode/backend/`):
   `openh264.rs`, `videotoolbox.rs`, `mediafoundation.rs`, `nvenc.rs`, `vaapi.rs`.
   Each currently returns `Vec<Bytes>`; wrap each `Bytes` in `Packet { payload,
   timestamp }` using the `timestamp` argument. For the vaapi backend, `encode` at
   `encode/backend/vaapi.rs:59` becomes
   `fn encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe: bool) ->
   Result<Vec<Packet>, Error>` and maps its output units to
   `Packet { payload, timestamp }`.

4. **The `Encoder` front-end** (`rs/moq-video/src/encode/encoder.rs`). `encode_raw`
   (`:255-264`) threads the timestamp to `self.backend.encode(frame, timestamp,
   keyframe)` and returns `Vec<Packet>`. The three public entry points that funnel
   through it (`encode_rgba` `:189-197`, `encode_i420` `:210-219`, `encode`
   `:249-251`) gain a `timestamp: Timestamp` argument and return `Vec<Packet>`; a
   caller that has a frame has its presentation time. `finish` (`:270-272`) returns
   `Vec<Packet>`.

5. **The `Sink` wrapper** (`rs/moq-video/src/encode/sink.rs`). The threaded and the
   inline `Sink::encode` (`:123`, `:187`) take `timestamp: Timestamp` and return
   `Vec<Packet>`; the threaded `Request::Encode` variant (`:38-45`) carries the
   timestamp down the channel alongside `frame` and `keyframe`, so it lands in order
   with the frames around it.

6. **The producer stamping site** (`rs/moq-video/src/encode/producer.rs:386-392`).
   Pass the frame's timestamp into `encode`, then publish each returned packet with
   its own timestamp instead of the single call-site `ts`:

   ```rust
   let ts = Timestamp::from_micros(clock.micros())?;
   let packets = encoder.encode(frame, ts, force_keyframe).await?;
   force_keyframe = false;
   catalog_ready |= !packets.is_empty();
   for packet in packets {
       producer.publish(vec![packet.payload], packet.timestamp)?;
   }
   ```

   This keeps `Producer::publish(Vec<Bytes>, Timestamp)`
   (`producer.rs:87-104`) unchanged, so the bring-your-own-Annex-B path and the
   pre-encoded-source concept (libcamera leaf) that rely on that public signature
   are untouched; the honesty comes from the loop using each packet's own timestamp.

## Implementation steps

1. Add the `Packet` type and `use moq_net::Timestamp;` to the encode backend module.
2. Change the `Backend::encode` and `Backend::finish` signatures
   (`encode/backend/mod.rs:37-57`). The `Candidate` table and `open`
   (`:60-134`) are untouched; only the trait method shapes change.
3. Update the five backends in one pass, each wrapping its `Bytes` in a `Packet`
   with the passed timestamp. This is mechanical; keep the diff per file to the
   `encode`/`finish` bodies.
4. Thread the timestamp through `encode_raw` and the three public `Encoder`
   entry points and `finish` (`encoder.rs:189-272`), returning `Vec<Packet>`.
5. Thread it through both `Sink` variants and the `Request::Encode` channel message
   (`sink.rs`).
6. Rewrite the producer stamping site (`producer.rs:386-392`) to pass the timestamp
   in and publish per-packet.
7. Update the in-crate encode tests and the round-trip test (`producer.rs:403+`,
   `encoder.rs` tests) to the new signatures; assert each returned `Packet.timestamp`
   equals the frame's for the zero-delay backends, proving behavior is unchanged.

## Tests

- Extend moq's existing `software_encoder_emits_annexb` (openh264) and the
  producer-level round trip so they assert the returned `Packet.timestamp` equals
  the input frame's timestamp for every zero-delay backend, which is the guarantee
  that B2 changes no observable behavior.
- A hardware-gated round-trip test for a pipelined backend belongs to the
  v4l2-encode leaf, not B2: that test submits a run of frames with monotonically
  increasing timestamps and asserts the drained packets carry the timestamps of the
  frames they encode, not the frames submitted at drain time. B2 only ships the
  seam and the zero-delay assertions.
- No ffmpeg: the openh264 encode-to-openh264-decode in-crate check stays the ground
  truth, exactly as moq does it today.

## Adaptation notes

- Timestamps are `moq_net::Timestamp` at the boundary, never `Duration`
  (`comparisons/moq-changes.md` section 3 item 1). Internally our OS-thread
  pipelines can keep `Duration` behind the seam, but no contributed signature does.
- The `Vec<Packet>`-per-frame return is what makes moq's one-shot `encode` shape
  capability-equivalent to a push/pop streaming encoder
  (`comparisons/traits-api.md` section 6): a pipelined backend returns zero packets
  while a frame is still in the device queue and several on a later drain, each
  correctly stamped. No change to the public one-shot `Encoder` model is needed
  beyond adding the timestamp argument.
- `Packet` is `#[non_exhaustive]`, so a future field (a DTS, a frame-type tag) stays
  additive.
- The public `Encoder::encode_rgba`/`encode_i420`/`encode` gaining a `timestamp`
  argument is the one externally visible ripple. It is behavior-preserving (the
  caller supplied the timestamp post-hoc at the producer before) but it is a public
  signature change, so confirm it in the base RFC. The alternative, keeping those
  signatures and collapsing `Vec<Packet>` back to `Vec<Bytes>` internally, would
  re-hide the timestamp from bring-your-own callers and defeats the point; prefer
  the explicit argument.

## Coordination

- Coordination point 1 (base API freeze): the `Backend::encode` signature and the
  `Packet` shape are frozen contract. The v4l2-encode and android-mediacodec leaves
  code against them and must not redefine them.
- No shared-file conflict: B2 touches the trait and the backends but not the
  `Candidate` tables, so it does not collide with the per-leaf candidate additions
  (coordination point 2).

## Transcode and rate control (overview coordination point 7)

Beyond live pipelined encode, the per-`Packet` timestamp and `finish()` draining
also serve one-shot FETCH transcoding: moq-transcode builds a fresh encoder per
fetched group, which must drain and stamp its packets correctly on `finish()`.
This reinforces that B2 is needed for per-group transcoding, not only for a
continuous stream.

## Acceptance checklist

- [ ] `Backend::encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe:
      bool) -> Result<Vec<Packet>, Error>` matches the frozen contract verbatim.
- [ ] `Packet { payload: Bytes, timestamp: Timestamp }` is `#[non_exhaustive]` and
      `timestamp` is `moq_net::Timestamp`.
- [ ] All five in-tree backends (openh264, videotoolbox, mediafoundation, nvenc,
      vaapi) compile and echo the current frame's timestamp.
- [ ] The producer publishes each packet with its own timestamp;
      `Producer::publish(Vec<Bytes>, Timestamp)` is unchanged.
- [ ] Existing encode and round-trip tests pass and assert timestamp echo for the
      zero-delay backends.
- [ ] No ffmpeg introduced; no `Duration` in any contributed signature.
