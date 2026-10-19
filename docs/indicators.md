# Indicator modules

The indicator browser is shared by candlestick, footprint and heatmap panes. It opens at window level, including native popouts, so a narrow chart cannot clip the menu. Search matches names, descriptions and groups. Selecting a row opens its editor; its checkbox explicitly enables or disables the indicator. The active view lets users drag panel indicators without moving price overlays. Search results cannot be reordered. Small windows use a list and a separate detail view with back navigation. Each scrolling region is independent. Bubble and GEX editors expose essentials first and retain advanced controls behind one checkbox.

## Registration and extension

- `data/src/chart/indicator.rs` defines stable serialized IDs and common placement APIs.
- `data/src/chart/indicator/catalog.rs` is the single catalog for labels, descriptions, groups, SVG families, calculation adapters, settings editors and placement. Market lists are derived from it. Every enum variant must have exactly one registration; tests enforce completeness and stable serialization.
- `src/chart/indicator/kline.rs` creates runtime adapters from the catalog instead of matching every indicator ID. Panel adapters implement the common `KlineIndicatorImpl` lifecycle. Overlays retain their existing chart rendering and use the same discovery and persistence path.
- `data/src/chart/kline/scripted.rs` adapts OHLCV bars to the shared upstream engine. Closed bars commit once; live bars preview without advancing committed state. History correction, pruning, basis and input changes reset/replay the engine. Missing values remain gaps.
- `src/indicators/model.rs` owns transient navigation, filtering and safe panel ordering. It does not persist search text or UI state in layouts.
- `src/indicators/mod.rs` renders the common browser. It contains no per-indicator configuration form.
- `src/indicators/editors/` contains independent settings editors. Existing typed configurations and the normal visual-config update path are preserved.
- `src/indicators/icons.rs` maps reusable icon families to embedded SVG assets in `assets/ui/indicators/`.

To add an indicator, add a stable ID and one catalog entry. Reuse a runtime adapter and editor when possible. A new script registers its source and typed inputs in the shared script adapter. A new native calculation implements the lifecycle trait and registers one runtime adapter. A custom editor lives in its own file and is selected through the catalog's editor kind. No pane-local popup, extra toolbar action or duplicated market list is needed. Keep enum and catalog order aligned; the registration test checks this invariant.

Saved-state recovery validates indicator IDs through their deserializer, so adding a registered ID does not require maintaining another string whitelist. Retired IDs are removed individually, preserving the remaining chart, streams, indicators, drawings and supported settings.

## CVD and retired settings

CVD executes the unchanged upstream `data/indicators/sources/cumulative-volume-delta.ind`. It accumulates buy minus sell volume over loaded history, draws a line by default and optionally draws per-bar change bars. Those bars have an independent scale and render behind the cumulative line. There is no daily reset and no inferred OHLC candle. Directional exchange volumes are preferred; complete trade-derived volume can supply venues without it, while only the live bar may use partial execution coverage. Missing directional data is not fabricated as zero.

Old CVD IDs stay valid. Old candle, wick and reset options are ignored; the upstream input defaults to change bars off. Smart Money Concepts has been removed from discovery, runtime and settings. Loading an old SMC selection recovers the rest of the layout and excludes only that retired indicator.

## Verification

Run the repository quality gate with all features and targets. Additional regressions cover catalog completeness, market lists, upstream CVD values across midnight, repeated live previews, corrections, missing directional volume, configuration changes, state recovery, search, activation idempotence and safe panel ordering.

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
$env:FLOWSURFACE_INDICATOR_QA = "target/indicator-ui-qa"
cargo test --workspace --all-targets --all-features --locked indicators::tests:: -- --ignored --nocapture --test-threads=1
```

The opt-in GPU test writes synthetic previews for every available editor, advanced controls and compact layouts. It checks popup bounds and sends pointer events to actual checkbox, row-selection and close widgets. These checks do not claim live feed availability. Saved-configuration checks in `src/config_verification.rs` use copied fixtures only.
