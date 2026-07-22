# catalog-adopt

> Campaign: align-to-moq | Kind: task plan | Branch: align/catalog-adopt |
> Read ../0-overview.md first.

Wave: 1
Depends on: Wave 0 pin bump (the moq release carrying moq-mux with `Reserved`/`Rendition` and per-rendition `Estimate`, `hang` with the `to_json` rename)
Coupling: independent

## Goal

A long-standing audit finding (plans/old/review-moq-usage.md finding 1) said
iroh-live hand-rolled the catalog stack that moq-mux already provides: a bespoke
`Catalog` with inlined video and audio copies, a snapshot consumer with a
group-advance race and no delta support, and a bespoke producer. That finding is
resolved. The round-2 comparison verified against current source that
`moq-media/src/catalog.rs` (75 LOC) is already the sanctioned shape, a
`CatalogExt` extension over `moq_mux::catalog::hang::Catalog` with the moq-mux
producer and consumer, and that `moq-media/src/publish.rs` wraps
`moq_mux::catalog::Producer` rather than reimplementing it. So this task is not a
rewrite of the catalog module; it is a precisely scoped adoption of the remaining
moq-mux catalog machinery that iroh-live still works around by hand, plus the
migration chores the `hang` bump forces. Chat and user stay exactly where they
are, as `CatalogExt` extension sections. The concrete gains: replace the
empty-catalog priming hack with moq-mux's `Reserved` initial-publish gating, and
adopt the per-rendition `Estimate` feed so advertised bitrate and jitter are
measured rather than asserted.

## Evidence

- The resolved finding, verified against source: `../upstream/comparisons/pubsub.md`
  section 4 ("Current state on our side"), which quotes `catalog.rs:9-27` and
  states "the current code is exactly the recommended shape ... Nothing to cut
  here; 75 LOC is the floor," and names the prior audit
  (`../../old/review-moq-usage.md` finding 1, 2026-06-18) as the origin of the
  hand-roll claim and marks it resolved. Confirmed by reading `catalog.rs`: the
  whole module is `type Catalog = moq_mux::catalog::hang::Catalog<IrohLiveExt>`,
  `type CatalogConsumer = ...hang::Consumer<IrohLiveExt>`, and
  `struct IrohLiveExt { chat, user } impl CatalogExt`.
- What remains to adopt: `../upstream/comparisons/pubsub.md` section 2
  (the priming hack at `publish.rs:578-585`, and the moq-mux `Reserved`
  machinery as its principled replacement), section 4 ("Field-level items that
  matter to us": the `to_json` rename, `#[non_exhaustive]` construction,
  `Duration` jitter typing, the `broadcast` cross-reference field), and the
  section 10 catalog verdict ("keep as is ... Migration chores on the bump").
- Deletion ledger: `../cut-plan.md` section 2, moq-media table, `catalog.rs`
  row: 75 LOC, verdict **keep**, "already the sanctioned `CatalogExt` shape; the
  floor." The related `publish.rs` row (verdict **merge**) names the priming hack
  "replaced by `Reserved` semantics" with prerequisite "release `Reserved`."
- Chat and user placement: `../upstream/comparisons/pubsub.md` section 4
  ("Where do chat and user belong?"), verdict (b) keep as a `CatalogExt`
  extension today, and section 8 (chat stays entirely ours).

## moq primitive adopted

The moq-mux generic catalog stack (`rs/moq-mux/src/catalog`), most of which
iroh-live already uses; the pieces to newly adopt:

- `moq_mux::catalog::Producer<E: CatalogExt>` reservation gating:
  `Producer::reserve` (`rs/moq-mux/src/catalog/producer.rs:156`) returns a
  clonable `Reserved` (`rs/moq-mux/src/catalog/tracks.rs:228`); the catalog is
  withheld from the broadcast until every reservation resolves, so a subscriber's
  first snapshot is the complete track list. This is the inverse and correct form
  of the current priming touch. `Producer::with_catalog`
  (`producer.rs:96`) and `Producer::lock` (`producer.rs:140`) are already used by
  `CatalogProducer`.
- `Rendition<E, C>` (`tracks.rs:293`) and the public `RenditionConfig<E>` trait
  (`tracks.rs:90`), the unsealed rendition layer, with the `Estimate { jitter,
  bitrate }` detector (`tracks.rs:16`) fed by `record_frame`/`record_reorder`/
  `record_group_end`, auto-filling only the catalog fields the config left absent
  (pubsub.md section 2). `VideoHint` (`tracks.rs:122`) carries caller-provided
  fields with stream-detected values winning.
- The `hang` catalog model: `hang::catalog::{Video, Audio, VideoConfig,
  AudioConfig}`, all `#[non_exhaustive]`, with `jitter: Option<Duration>` present
  on both configs, `broadcast: Option<moq_net::PathRelativeOwned>` cross-reference,
  and the `to_json`/`to_json_pretty` rename replacing `to_string`
  (pubsub.md section 4).

## iroh-live code changed

- `moq-media/src/catalog.rs` (75 LOC): keep as the floor. The only edits are the
  migration chores the bump forces: any `to_string`-on-catalog call site becomes
  `to_json` (the Deref-to-`to_string` gotcha has bitten before, per project
  memory and pubsub.md section 4), `#[non_exhaustive]` construction of `Video`
  /`Audio`/`VideoConfig`/`AudioConfig` where iroh-live builds them, and awareness
  of the `broadcast` cross-reference field when handling renditions. Chat, user,
  and the `CatalogExt` impl are unchanged.
- `moq-media/src/publish.rs`: the `CatalogProducer` wrapper (`publish.rs:572-585`)
  keeps wrapping `moq_mux::catalog::Producer<IrohLiveExt>`, but the empty-catalog
  priming hack (`publish.rs:578-585`, the "touch it once to publish the initial
  empty catalog" `producer.lock().video = Video::default()`) is replaced by
  `Reserved` gating: reserve the renditions the simulcast registry will start,
  and let moq-mux publish the first complete snapshot when they resolve. The
  `VideoRenditions`/`AudioRenditions` registry and `SharedVideoSource`
  (pubsub.md section 2) stay; only the initial-publish sequencing changes.
- Per-rendition `Estimate` feed: where iroh-live advertises static preset bitrate
  and never populates `jitter` (pubsub.md section 2, "a real gap on our side"),
  wire the importer-fed metrics so the published catalog carries measured bitrate
  and jitter. If iroh-live keeps its own encoders (the codec-layer question is a
  separate upstream-gated task), replicate the `record_frame`/`record_group_end`
  feed; if the encode layer moves onto `moq_video`/`moq_audio` producers, the feed
  comes for free. Scope this task to the catalog-side wiring only.
- The dynamic-handler registration race workaround (`publish.rs:246-252`) is a
  moq-net ordering sharp edge, not a catalog concern; note it for an upstream
  report but do not fold it into this task.

What is explicitly out of scope, because it is already aligned: the `Catalog`
/`CatalogConsumer` type aliases, the `IrohLiveExt` extension, the `CatalogExt`
impl, and the `CatalogProducer`'s use of `moq_mux::catalog::Producer`. There is no
bespoke catalog to delete; the audit finding it described is closed.

## Steps

1. Confirm the pin bump (Wave 0) landed, so moq-mux with `Reserved`/`Rendition`
   and the per-rendition `Estimate` detector, and `hang` with `to_json`, are
   available. Re-diff the moq-mux catalog surface against the pinned release, since
   `../../upstream/cut-plan.md` risk R-b warns the `to_json` rename class of change and the
   `#[non_exhaustive]` sweep land at the bump.
2. Migration chores first (they unblock compilation): `to_json` rename at every
   catalog-to-string site, `#[non_exhaustive]` construction, `Duration` jitter
   typing. `refactor:` commit, docs riding the code (../cut-plan.md section 5).
3. Adopt `Reserved` gating in `CatalogProducer`: reserve the initial renditions,
   drop the priming touch, and verify an early subscriber receives a complete
   first snapshot rather than an empty one. `feat:`/`refactor:` commit; delete the
   priming hack only once the proof below passes.
4. Wire the per-rendition `Estimate` feed so published catalogs carry measured
   bitrate and jitter; verify the fields appear in a produced catalog.
5. Leave chat and user untouched; confirm the `catalog.rs` extension test
   (`ext_flattens_into_catalog`, `catalog.rs:47-75`) still passes.

## Proof before deletion

Coordination point 1 gate for the priming-hack removal:
`moq-media/tests/pipeline_integration.rs` and `iroh-live/tests/e2e.rs` must pass
on the `Reserved`-gated path before the priming touch (`publish.rs:578-585`) is
deleted. Add or extend a test that subscribes to the catalog track before the
first rendition frame is produced and asserts the first snapshot is the complete,
non-empty track list (this is the exact behavior `Reserved` provides and the hack
approximated). The `catalog.rs` unit test `ext_flattens_into_catalog` guards that
chat and user still flatten alongside video and audio.

## Coordination

- No zero-copy path is touched (coordination point 2 does not apply).
- The priming-hack removal shares a boundary with the `pubsub-align` task's
  subscribe-side work and with the broader publish.rs `encode::Producer` collapse,
  which is upstream-gated (stage A1, ../cut-plan.md); keep this task to the catalog
  produce side and the `Reserved` gating so it stays independent of the codec
  adoption.
- Do not pursue moving chat and user upstream into hang (pubsub.md section 4
  verdict (a) is explicitly rejected: upstream keeps the root model media-only).
  Moving identity out of the catalog entirely is a room-layer decision
  (`rooms-announce`, Wave 2), not this task.

## Acceptance checklist

- [ ] `catalog.rs` stays at its 75-LOC floor with chat and user as `CatalogExt`
      extension sections; no bespoke catalog code is introduced or was found to
      delete.
- [ ] The empty-catalog priming hack (`publish.rs:578-585`) is replaced by
      moq-mux `Reserved` gating, and an early subscriber gets a complete first
      snapshot.
- [ ] Published catalogs carry measured bitrate and jitter via the per-rendition
      `Estimate` feed instead of static preset values.
- [ ] `to_json`, `#[non_exhaustive]` construction, and `Duration` jitter typing
      migration chores are applied.
- [ ] `pipeline_integration.rs`, `e2e.rs`, and the `catalog.rs`
      `ext_flattens_into_catalog` test pass; `cargo make check-all` is green.
