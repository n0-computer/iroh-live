# Per-segment transcoding and FETCH: the rate-control rule for encoders

> Campaign: upstream (media stack) | Kind: cross-cutting note | Read
> `../overview.md` first. This constraint applies to every encoder module, so it
> lives here rather than in one module.

moq's stated codec direction is per-group (per-segment) transcoding with FETCH
support: a FETCH for group 45 of a lower rendition transcodes that one group
from the source (held in relay memory, possibly disk) down to, for example,
360p, with custom per-GOP rate control. moq-transcode already owns this policy.

Our encoder contributions plug into it with no integration work, because
moq-transcode drives encoders only through the public
`encode::{Kind, Config, Encoder}` front end: it selects by `Kind`, sets a
per-rung CBR target through `Config.bitrate` at construction, forces an IDR per
group, and builds a fresh encoder per fetched group. It never uses
`rate::Control`. Our zero-copy VAAPI decode into VPP scale into VAAPI encode is
the Intel and AMD analog of moq-transcode's NVDEC-to-NVENC path
(`../modules/codec-vaapi-encode.md`), and rav1e with rav1d is the software
fallback, so both slot in by `Kind` selection.

## The rule this imposes on every encoder module

Expose per-segment rate-control primitives and defer the rate-control policy to
moq-transcode. Never embed a streaming rate controller in a backend.

- An honest `set_bitrate` that succeeds or returns `Error::BitrateUnsupported`,
  with no forced-IDR side effect on retune.
- A per-encode target-bitrate or QP knob.
- Forced IDR per GOP, on demand.
- Cheap reconfigure or session reuse between groups.

## The per-group re-open cost each encoder must address

moq-transcode builds a fresh encoder per fetched group, so the construction cost
matters:

- rav1e is cheap to construct.
- VAAPI opens a VA context; the VAAPI encode module keeps a session-reuse path
  rather than constructing fresh per group (`../modules/codec-vaapi-encode.md`).
- V4L2 is the most expensive (full device open plus REQBUFS plus STREAMON), so
  the V4L2 encode module adds a session-reuse path with an explicit
  `EncoderCmd::Reset` between groups (`../modules/codec-v4l2-encode.md`).

The evidence is `../comparison/moq-inventory.md` (moq-transcode) and
`../comparison/moq-changes.md`.
