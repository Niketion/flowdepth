# Upstream 0.8.10 integration into Flowdepth

Review baseline: `c1388d4f6765b56ce02783981311cf46435e5c48` (the last upstream commit already in Flowdepth).
Reviewed upstream tip: [`0a0416bcae8c34836fc8b59985fd82cebffedbe8`](https://github.com/flowsurface-rs/flowsurface/commit/0a0416bcae8c34836fc8b59985fd82cebffedbe8).
Flowdepth integration base: `3997801d7ec74a912fbe739f62d0f98e654d4ebe` (`origin/develop` at review time).

This is a **selective source integration**, not a full merge of the new chart application. The full upstream renderer, arbitrary chart composition, indicator browser and its layout schema are not enabled. Git ancestry is intentionally not marked as fully merged: later synchronization must still review those changes. The accompanying [file inventory](upstream-0.8.10-files.csv) accounts for all 270 paths changed between the upstream baseline and reviewed tip.

## Decisions across the seven upstream commits

| Upstream commit | Change | Integration decision |
| --- | --- | --- |
| `1d76c02` | MSIX, Linux installation and macOS bundling | Adopt fail-fast shell setup, input validation and locked builds. Keep Flowdepth portable package names and output locations, which are consumed by its develop release workflow. Do not use the upstream signing identity, installer identity or release workflow. |
| `a5df6b2` | Disable multi-window on Windows | Keep Flowdepth's pinned iced/winit revision, Windows popouts and fallback/recovery behavior. A blanket disable would remove a fork capability supported by its backend patch. |
| `6fc2fe4` (#143) | Chart composition and new rendering groundwork | Retain Flowdepth dashboard, drawing tools and existing chart lifecycle. The new architecture is useful for multi-series composition, but replacing the complete UI would disconnect GEX/news and custom workflows. No unused intermediate WebSocket connection module is copied. |
| `0774cd0` (#144) | Unified charts, split model/view, new persistence | Keep the Flowdepth layout schema, state recovery, persistent cache and complete/partial trade evidence. Adopt compatible exchange changes and quantity fixes. Do not silently migrate customized layouts to the incompatible unified schema. |
| `cc8dc08` (#146) | Indicator language/compiler/runtime | Adopt the upstream indicator-engine crate, compiler, runtime limits, CLI, documentation and licenses. It is part of the workspace and exercised by the normal quality gate. |
| `d0b379e` (#147) | Source-defined indicators and catalog | Adopt the upstream source sets. Connect RSI, ATR, MACD and ADX/DMI to existing chart panels, including adjustable periods, live previews, replay after history changes, multiple output lines, MACD histogram and tooltips. Validate both full source sets in CI. The arbitrary-source GUI browser and indicator chaining require the unified chart UI and are not included. |
| `0a0416b` (#148) | TPO, profiles, bounded price grids and paged candles | Port the TPO calculation to `data/src/chart/kline/tpo.rs` and expose a candlestick overlay. Integrate upstream KlinePage across all five existing venues and use it in cache gap fetching. Keep Flowdepth SVP/fixed-range profiles and adaptive volume bubbles. |

## Overlapping features

| Area | Kept implementation and reason |
| --- | --- |
| CVD | Flowdepth. Preserves daily/continuous reset, candle/line display, directional-volume availability and real intrabar high/low only when complete trade sequences prove them. The generic upstream source is not substituted for this behavior. |
| Footprint / bar analysis | Flowdepth. Preserves table presentation, directional clusters, summaries, imbalance controls and raw-trade coverage. The new indicator engine is available without replacing this specialized table renderer. |
| Volume bubbles / big trades | Flowdepth adaptive clustering, percentile thresholds, stable identities, price-response coloring, historical summaries and retention. Upstream Big Trades is not an equivalent replacement. |
| Session and fixed-range volume profiles | Flowdepth. Preserves partial-coverage feedback, cached execution data, independent drawing configuration and hourly retention fixes. Upstream profile calculation supports the new composition model and would require a dedicated conversion of these semantics. |
| VWAP | Flowdepth native overlay and its history/coverage handling. Source-defined VWAP is retained in the upstream source set for CLI validation; it is not presented as a second competing GUI VWAP. |
| GEX, Derive maker flow, options providers, Smart Money Concepts, News Tracker | Flowdepth. These have no complete upstream replacement in the reviewed changes. |
| Market-data ingestion | Combined. Keep trade IDs, deduplication, invalid-execution filtering, order-flow events, MEXC recent trades, extended metadata timeout and cache invalidation. Add upstream venue-aware candle pages, API status/row validation and Arrow cursor advancement. |
| Theme, fonts, toolbar, templates and startup | Flowdepth. Preserve its asset/codepoint mappings, GEX/news controls, startup feedback and layout copy/import/export. |

## New behavior

RSI, ATR, MACD and ADX/DMI are available in the existing indicator selector for both spot and perpetual markets, on time and tick charts. Parameters live in the existing chart configuration and round-trip with saved layouts. Missing fields in old layouts use defaults.

The upstream scripts themselves are kept unchanged. A small adapter supplies OHLCV frames to the upstream engine. Closed bars are committed once; the current bar is previewed from the last committed state. Backfill, historical corrections, pruning, parameter changes and switching basis reset/replay state. Missing warm-up values break drawn lines instead of becoming zero-valued signals. MACD's histogram includes zero in the fitted scale.

TPO is available as a candlestick overlay with daily, weekly or monthly UTC sessions, configurable 15m/30m/1h/2h/4h letters, value-area percentage and optional letters. It uses loaded candle high/low ranges rather than claiming exact execution-level price visits. Candles must be no longer than the letter period. Tick charts do not produce candle-based TPO. Complete/developing/partial session status comes from the upstream coverage calculation. Only sessions intersecting the visible range are drawn, capped at 32; the display grid is bounded to 256 rows per session. Automatic loading of an independent fine-timeframe TPO source, interactive session splits/merges and the full upstream profile inspector are not included.

Candle requests with explicit ranges now page through venue limits, even when a page is empty. Invalid/non-advancing cursors fail instead of looping. Filtering uses half-open page bounds and keeps the candle containing a non-aligned request start. Cache coverage is still recorded only for contiguous candles actually returned. An empty network page does not prove stored market-data coverage.

The Arrow cursor now advances to the later timestamp when an earlier batch contained null values and a later batch contained valid trades. Quantity rounding uses the upstream symmetric signed rounding and bounded saturation. Price-grid operations use upstream checked addition and bounded coarsening while retaining the existing drawing helper API.

## Validation and reproducibility

Required gates remain those documented in CONTRIBUTING.md:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo run --locked -p flowsurface-indicator-engine --bin flowsurface-indicator -- test data/indicators/sources --namespace core
cargo run --locked -p flowsurface-indicator-engine --bin flowsurface-indicator -- test indicators/sources --namespace catalog
```

Added regression tests cover repeated open-bar updates, incremental/replay equivalence, historical correction, pruning, input changes, tick basis, half-open pages, empty-page progression, invalid cursors, failed pages, signed quantity rounding, TPO coverage, TPO row bounds and calendar boundaries. Existing tests continue to cover Flowdepth state migration, custom charts, cache, trade identity and recovery.

Local verification is performed on Windows. Cross-platform release packaging and live exchange connections are outside the local test evidence; CI still builds the existing Windows, Linux and macOS artifacts. No credentials, market-data caches or user layouts are migrated by this integration.

The original worktree's uncommitted Hyperliquid/Paradex work is preserved on `feat/hyperliquid-orderflow-depth`; it is not silently folded into this develop integration. The integration worktree lives under `target/upstream-realign`.

## Completed local validation (2026-10-16)

- Formatting, workspace check and Clippy with `-D warnings`: passed.
- Workspace tests with all targets/features and the lockfile: 417 passed, 0 failed, 7 explicitly ignored by the suite.
- Official CLI smoke tests: all 9 core and 27 catalog sources passed.
- Imported engine/core/catalog trees match the reviewed upstream source exactly.
- Modified release shell scripts pass Bash syntax checks and reject unsupported architectures before building.
- The file inventory covers all 270 changed upstream paths with no duplicate or missing entries.

The complete chart rewrite remains outside this selective integration; this is not a claim of complete UI parity with upstream 0.8.10.
