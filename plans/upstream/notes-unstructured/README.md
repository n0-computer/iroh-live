# notes-unstructured

> Campaign: upstream (media stack) | Kind: index | Read ../overview.md first.

Material that does not belong to any single module doc lives here.

- [transcode-and-fetch.md](transcode-and-fetch.md) - moq's per-segment
  transcoding and FETCH direction, and the rate-control rule it imposes on every
  encoder module.
- [coordination.md](coordination.md) - the cross-cutting coordination concerns:
  the external moq-vaapi repo and its dependency-spine open question, licensing
  and provenance of ported FFI, CI hardware gating, semver across the fan, and
  the B4 Android-placement open question.
- [staging-and-risks.md](staging-and-risks.md) - the stage ordering (M0 type
  convergence, M1 codec adoption atomic-per-platform, M2 capture adoption), the
  upstream-gated cuts, and the full risk register (release timing, API churn and
  the plan-freshness protocol, acceptance, the rav1d and cpal pins, behavioral
  deltas, and the platform verification gate).
- [branches.md](branches.md) - the branch registry: the `up/<name>` moq branch
  and its iroh-live pair per module.
- [parity-ports.md](parity-ports.md) - the port-our-fixes register for the
  adopt-theirs backends: where our version carries a fix moq lacks, upstreamed
  before the local code is cut.
- [analysis/](analysis/) - the wider iroh-live-to-moq refactor analysis these
  plans were drawn from, preserved for context and superseded by the current
  campaign docs where they disagree.

If a future note has no better home, add it here and link it from this index.
