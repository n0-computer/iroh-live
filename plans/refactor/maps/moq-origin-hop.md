# PARTIALLY STALE - use maps/moq-net-origin.md instead

The MAIN-branch content here (moq-net Origin/Path/announce/subscribe
model) is roughly valid, but the DEV content described an abandoned
April-era experimental line (`moq-lite` rename, the #1152/#1142/#1134
PRs in their original form). Current dev (`261c2048`) net layer is
main plus newer refactors (#2176 latency_max, #2302 Session/Driver,
#2241 subscription migration, #2348 stats, #2307 announce), not that
line.

Current, correct net/origin map: `maps/moq-net-origin.md`.
