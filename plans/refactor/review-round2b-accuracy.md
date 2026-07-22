# Review round 2b: accuracy and consistency

Scope: numbered docs 0-10 (incl. 3t, 3u, 3z) under `plans/refactor/`, checked
against moq working tree `/home/bit/Code/rust/moq` HEAD `3a3e0ea8` (dev merged
into main 2026-07-21) and iroh-live at `/home/bit/Code/rust/iroh-live`. No files
were changed. Verdicts: LEAK / WRONG / INCONSISTENT / STALE / CONFIRMED-OK.

## 1. Dev/main framing (every hit adjudicated)

| # | doc:section | issue | verdict | evidence | severity |
|---|---|---|---|---|---|
| 1 | 0-overview:19-23 | "moq merged its dev branch into main... dev branch is now behind main and defunct" | CONFIRMED-OK | factual merge-note | - |
| 2 | 0-overview:77 | checklist item "remove all main-vs-dev language" | CONFIRMED-OK | describes the rewrite task | - |
| 3 | 3-compare-codecs:10,597 | "the dev line merged into main on 2026-07-21" | CONFIRMED-OK | merge-note | - |
| 4 | 4-compare-capture:5 | "single branch since the dev line merged into main" | CONFIRMED-OK | merge-note | - |
| 5 | 6-compare-audio:6 | "since the dev line merged into main on 2026-07-21" | CONFIRMED-OK | merge-note | - |
| 6 | 3u:16 | "no longer a dev line to target: everything... against moq main" | CONFIRMED-OK | merge-note | - |
| 7 | 7-cut:4-5 | round 1 called it "dev-only", now simply moq main | CONFIRMED-OK | merge-note | - |
| 8 | 7-cut:18-21 | round-1 three-world model incl. "thin main-only ~1,300 LOC" and "dev stack ~4,800"; "World (i) no longer exists" | CONFIRMED-OK | explicitly retired historical framing | - |
| 9 | 7-cut:50-51 | round-1 "main-compatible versus dev-dependent" split "is gone" | CONFIRMED-OK | merge-note | - |
| 10 | 7-cut:215 | "No stage is dev-dependent anymore" | CONFIRMED-OK | merge-note | - |
| 11 | 7-cut:372-389 | R-a risk retired: "shipped only on a dev branch"; "unmergeable dev line" | CONFIRMED-OK | describes retired round-1 risk | - |
| 12 | 8-upstream:4 | "no dev line to target" | CONFIRMED-OK | merge-note | - |
| 13 | 9-room:13-14,499 | "earlier drafts called dev-only are now plain moq main"; "removes the main-vs-dev split"; "no longer main-compatible vs dev-dependent" | CONFIRMED-OK | merge-notes | - |

No surviving live comparative dev-vs-main framing found in the numbered docs.

## 2. Key deltas vs current moq source

| # | doc:section | claim | verdict | evidence | severity |
|---|---|---|---|---|---|
| 14 | 10-summary:159, 2-inv:433, 9-room | `create_broadcast(path, Route)` replaced `publish_broadcast` | CONFIRMED-OK | `origin.rs:869 pub fn create_broadcast(path, route: broadcast::Route)`; `Route::announced()` at broadcast.rs:112; grep `publish_broadcast` = 0 hits | - |
| 15 | 2-inv:436,478; 9-room:172 | ROUTE_LINGER removed / unannounce synchronous on last detach (#2419) | CONFIRMED-OK | `ROUTE_LINGER` grep = 0 hits; origin.rs `detach_source` closes broadcast synchronously when routes empty | - |
| 16 | 2-inv:443; 9-room:164-172; 10-sum | moq-token path-scoping (`Scope`, #2416) | CONFIRMED-OK | `moq-token/src/claims.rs:16 struct Scope {root,put,get}`, `Scope::allows`; `key.rs` `with_scope`/`validate_scope` | - |
| 17 | 2-inv:447,481; 10-sum:149 | moq-mux 0.7.6 `RenditionConfig` + `Estimate{jitter,bitrate}` auto-detect | CONFIRMED-OK | moq-mux `tracks.rs:16 Estimate{jitter,bitrate}`, `:90 trait RenditionConfig`, `Metrics`-backed `resolved()`; Cargo.toml 0.7.6 | - |
| 18 | 2-inv:460 | moq-stats `Traffic` has `fetches` + `datagrams` | CONFIRMED-OK | `moq-net/src/stats.rs:249 fetches`, `:259 datagrams`; moq-stats Cargo.toml 0.1.0 | - |
| 19 | 7-cut:90; 3t 4.1 | hang 0.19.5 displayRatio->displayAspect breaks rusty-codecs config.rs mirror | CONFIRMED-OK | hang video/mod.rs `display_aspect_width` with `alias="displayRatioWidth"`; rusty-codecs config.rs:170 reads `h.display_ratio_width` (compiles only vs pinned 0.19.1, breaks at 0.19.5) | - |
| 20 | 3u:37-44; 3t; 10-sum:60 | moq-video decode `Backend` still `pub(crate)`; `frame::Frame` lacks DmaBuf/AHardwareBuffer | CONFIRMED-OK | `decode/backend/mod.rs:67 pub(crate) trait Backend`; `frame.rs:23 pub(crate) enum Frame` variants Surface/Texture/Cuda/I420 only; Cargo.toml 0.0.6 | - |
| 21 | 2-inv:449,480; 8-up:308-314 | runtime `set_latency` on the container consumer (consumer.rs:479) | CONFIRMED-OK | moq-mux `container/consumer.rs:479 pub fn set_latency(&mut self,...)`, `:161 discontinuity()`. Note: docs correctly locate this on moq-mux's container consumer, not moq-video's decode consumer (which only forwards `latency_max` to `with_latency` at construction — the residual C10 ask) | - |

## 3. Number consistency

| # | doc:section | check | verdict | evidence | severity |
|---|---|---|---|---|---|
| 22 | 1-code-map:168, 10-sum:14, 7-cut:14 | 41,564 LOC denominator | CONFIRMED-OK | identical in all three; 1-code-map category table totals 41,564 | - |
| 23 | 10-sum:18-20, 7-cut:181 | Scenario A ~4,800 (12%), B ~17,400 (42%) | CONFIRMED-OK | identical figures and percentages | - |
| 24 | 10-sum:121-127 | per-crate "Today": 22,310 + 5,507 + 11,441 + 572 + 1,734 | CONFIRMED-OK | sums to exactly 41,564; rusty-capture 5,507 matches 1-code-map capture-backend line | - |
| 25 | 10-sum:26-27, 8-up:520 | upstream ~9,500-10,500 LOC / ~20 PRs | CONFIRMED-OK | 8-up:520 "roughly 9,500-10,500 LOC over about 20 PRs" | - |
| 26 | 2-inv:19-22, 5-pubsub:29 | versions: moq-video 0.0.6, moq-audio 0.0.9, moq-nvenc 0.0.1, moq-transcode 0.0.1, moq-stats 0.1.0, moq-net 0.1.18, moq-native 0.18.3, hang 0.19.5, moq-mux 0.7.6 | CONFIRMED-OK | all nine match Cargo.toml on `3a3e0ea8`; pinned line (0.1.11/0.17.1/0.19.1) consistent everywhere. Note: 0.18.3 and 0.7.6 correctly updated from round-1's 0.18.2/0.7.5 | - |
| 27 | 3t:779-871 | D1-D12 count (decision list) | CONFIRMED-OK | 3t section 8 lists exactly D1 through D12 | - |
| 28 | 3u:9; 8-up:101-388 | "contribution catalog C1 through C14" | CONFIRMED-OK | 8-upstream has exactly C1..C14 (14 `### Cn.` headers) | - |
| 29 | 3u | 5 sections, Section 5 change-list of 12 items | CONFIRMED-OK | Sections 1(1a,1b),2,3,4,5; change table numbered 1-12 | - |

## 4. Cross-references

| # | doc:section | issue | verdict | evidence | severity |
|---|---|---|---|---|---|
| 30 | 3u:13,68,186 | cites `maps/moq-dev-video.md` as a live evidence source 3x | LEAK | that file is a SUPERSEDED stub ("See maps/moq-video.md"), and 0-overview:103 lists `moq-dev-video` among retired stubs; the live map is `maps/moq-video.md`. Broken/retired-stub reference | substantive |
| 31 | 3u:10,46,114,220,243,258,265,301,366,515,537,584,607,609 | refers to 3z as "requirements R1 through R7" and uses R1/R4 throughout | STALE | 3z:425 states "This list supersedes the earlier R1 through R7 enumeration" and defines U1-U4; every other doc (7-cut:41, 8-up:19, 10-sum, 3z §5) uses U1-U4. 3u's R-numbers are dangling (not defined anywhere) and don't map 1:1 (3u "R4" = part of 3z U2) | substantive |
| 32 | 7-cut:58, 379-380 | asserts "2-moq-inventory.md summary table 2 is still written in the pre-merge dev/main framing" | STALE | 2-inv's summary tables (`:428` "capabilities on main" with "Present on moq main" column; `:464` "pending the next release") are fully reframed to main; 2-inv:13 says the split "is gone". 7-cut's characterization no longer holds (conclusion unaffected) | substantive |
| 33 | 7/8/10 -> 3u | "3u section 1b/2/4/5", "3u#1..#8", D1-D12, U1-U4, C1-C14 references | CONFIRMED-OK | all resolve: 3u sections 1b/2/4/5 exist; #1-#12 = Section 5 change list; D1-D12 = 3t §8; U1-U4 = 3z §5; C1-C14 = 8-up | - |
| 34 | numbered docs | no live reference to retired stubs (moq-main-media, moq-dev-media, moq-origin-hop, moq-dev-audio-nvenc, moq-dev-transcode-stats) except 0-overview's own "retired stubs" note | CONFIRMED-OK | only exception is #30 (moq-dev-video in 3u) | - |

## 5. Stale round-1 residue

| # | doc:section | phrase checked | verdict | evidence | severity |
|---|---|---|---|---|---|
| 35 | all | "the floor is ~1,300 LOC" as a live claim | CONFIRMED-OK | absent; 7-cut:19 mentions ~1,300 only as retired round-1 world (i); 8-up:471 "~1,300 LOC across 4 PRs" is an unrelated Wave-4 upstream total | - |
| 36 | all | "dev ships" / "next breaking dev release" / "eleven dev-only enablers" / "phase 3 dev-gated" | CONFIRMED-OK | none present in numbered docs (only in prior review-*.md); 2-inv:467 says "next breaking release" (not "dev release") | - |
| 37 | 3u | R1-R7 enumeration residue | (see #31) | STALE | - | substantive |

## Counts

- Total rows: 37
- CONFIRMED-OK: 34
- LEAK: 1 (#30)
- STALE: 3 (#31, #32, #37 — #37 is the same issue as #31)
- WRONG: 0
- INCONSISTENT: 0 (folded into STALE)

By severity (non-OK findings): substantive 3 (distinct issues #30, #31/#37, #32);
critical 0; nit 0.

Distinct actionable issues: 3.
1. (substantive, LEAK) 3u cites retired stub `maps/moq-dev-video.md` x3 -> should be `maps/moq-video.md`.
2. (substantive, STALE) 3u uses R1-R7 requirement numbering that 3z superseded with U1-U4.
3. (substantive, STALE) 7-cut:58/379 claims 2-inventory's summary table is still dev/main-framed; it is reframed to main.
