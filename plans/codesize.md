# Code size reduction — remaining items

D1 (scale_if_needed), D2 (h264 config), D3 (DynamicVideoDecoder macro),
D5 (NV12 render pass), D8 (libcamera cfg gate), D9 (dead conversion
functions) are complete (~510 lines reduced).

## Open items

| ID | Description | Savings | Effort | Risk |
|----|-------------|--------:|--------|------|
| D4 | RGBA/BGRA conversion pairs — parameterize by `PixelFormat` | ~80 | small | none |
| D6 | `DiscoveredVideoSource` in split.rs + viewer.rs — move to shared location | ~45 | trivial | none |
| D7 | `show`/`show_publish` overlay duplication — parameterize rendering core | ~80 | small | low |
| D10 | Dead `Nv12Data` fields — simplify if unused | ~10 | trivial | none |
| D11 | Fallback capture backends (nokhwa + xcap, 414 lines) — decision needed on platform support scope | ~414 | small | medium |
| D12 | Apple-specific types in `format.rs` — move to `format/apple.rs` submodule | 0 (readability) | small | none |

D6 is the only trivial remaining item. D4 and D7 are small cleanup tasks.
D11 requires a decision about whether nokhwa/xcap fallbacks are still needed.
