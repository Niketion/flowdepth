# Flowdepth — crypto order flow, below the surface

> Flowdepth is an open-source native desktop terminal for crypto order flow and options analytics, built in Rust as an advanced fork of Flowsurface.

[![Develop CI](https://github.com/Niketion/flowdepth/actions/workflows/develop-ci.yml/badge.svg?branch=develop)](https://github.com/Niketion/flowdepth/actions/workflows/develop-ci.yml)
[![Latest GitHub release](https://img.shields.io/github/v/release/Niketion/flowdepth?include_prereleases&label=latest%20beta)](https://github.com/Niketion/flowdepth/releases)
[![License: GPLv3](https://img.shields.io/badge/license-GPLv3-blue.svg)](./LICENSE)
[![Rust 1.95](https://img.shields.io/badge/Rust-1.95-orange.svg?logo=rust)](./rust-toolchain.toml)
[![Made with iced](https://iced.rs/badge.svg)](https://github.com/iced-rs/iced)
[![Upstream Discord](https://img.shields.io/badge/Discord-upstream%20community-5865F2.svg?logo=discord&logoColor=white)](https://discord.gg/RN2XAF7ZuR)

Flowdepth combines crypto order-flow charts with footprint and L2 heatmap views, adaptive volume bubbles, Binance iceberg/replenishment detection, Deribit GEX, and Derive observed maker flow. A persistent market-data cache, automatic reconnect, and historical gap recovery help charts remain useful through longer sessions and network interruptions.

> [!IMPORTANT]
> **Beta status:** Flowdepth is experimental but usable beta software and is evolving quickly. Back up important layouts before updating. Beta releases may contain regressions.
> Flowdepth is derived from and continues to credit the upstream [Flowsurface](https://github.com/flowsurface-rs/flowsurface) project.

<div align="center">
  <img
    src="https://github.com/user-attachments/assets/8ebf2bc1-1a4b-48cc-a108-348a6ef58825"
    alt="Flowdepth desktop terminal showing crypto market charts"
    style="max-width: 100%; height: auto;"
  />
</div>

## Why Flowdepth?

Flowdepth extends Flowsurface for traders and market observers who want deeper crypto order-flow tooling in a native desktop application. Its development focuses on richer execution and liquidity visualization, resilient market-data ingestion and recovery, and crypto-options analytics while preserving the open-source Rust foundation of the upstream project.

Detection and analytics such as possible iceberg activity, GEX, and observed maker flow describe market data; they are not certain trading signals.

## Download

Automated beta builds are available from the [Flowdepth Releases page](https://github.com/Niketion/flowdepth/releases). They are generated from the `develop` branch after the full quality gate succeeds.

For a quick start:

1. Open the [Releases page](https://github.com/Niketion/flowdepth/releases).
2. Download the package for your platform: Windows x64, Linux x64, or universal macOS.
3. Extract the archive.
4. Run `flowsurface.exe` on Windows or the `flowsurface` binary on Linux/macOS.

The application binary and Rust package are currently still named `flowsurface` for upstream compatibility.

Windows and macOS beta binaries are not currently signed. Windows SmartScreen may require **More info → Run anyway**. On macOS, control-click the binary and choose **Open**, or allow it under **System Settings → Privacy & Security**.

### Release channels

- **Beta builds:** automated builds from `develop`, published as GitHub prereleases after formatting, check, Clippy, tests, and all platform packages succeed. Tags use `develop-<commit>`, where `<commit>` is the seven-character Git commit identifier used for the build.
- **Stable releases:** not currently available.

## Key features

- **Order-flow charts:** footprint views, historical L2 heatmaps, Time & Sales, DOM/ladder, candlesticks, comparisons, volume profiles, and cumulative volume delta.
- **Adaptive volume bubbles:** configurable clustering of aggressive executions with adaptive thresholds, side/delta coloring, bounded labels, and stable live updates.
- **Possible Binance iceberg/replenishment detection:** optional markers for evidence of passive absorption on Binance USDⓈ-M perpetuals. The detector is probabilistic, disabled by default, and does not prove that an exchange-native iceberg order exists.
- **Crypto options analytics:** BTC and ETH GEX profiles sourced from Deribit, plus independently displayed Derive observed maker flow matched to Deribit contracts.
- **Persistent market-data cache:** local storage for klines, open interest, trades, and derived bubble summaries with gap detection, deduplication, and invalidation.
- **Connection recovery:** automatic reconnect, historical gap backfill, bounded retries, stale-request cancellation, and sequence-aware recovery.
- **Workspace tools:** persistent layouts, saved-state recovery, customizable themes, pane linking, loading/coverage feedback, and an in-app debug terminal.
- **Public market data:** market data is received directly from exchange REST APIs and WebSockets. Current features do not require exchange trading credentials.

### Historical trades on footprint charts

Live trades are captured over WebSocket. Binance charts can optionally backfill the visible range from [data.binance.vision](https://data.binance.vision/) daily files, the paginated REST API, or both. Historical trade fetching for Bybit and Hyperliquid is not available; OKX support remains work in progress.

By default, Flowsurface captures and plots live trades in real time via WebSocket.

To backfill the visible time range on footprint charts, enable **trade fetching**
in Settings → Network. Three modes are available:

- **Exchange**: Fetches directly from Binance's public REST API and bulk data
  mirrors ([data.binance.vision](https://data.binance.vision/)). Fast for
  daily archives; paginated and rate-limited for intraday ranges. **Binance
  only for full history** (spot, linear, inverse). MEXC perpetuals can seed the
  chart from the public recent-deals window (the latest 100 trades); use Server
  mode for older MEXC history.

- **[Server](https://github.com/flowsurface-rs/server)**: Fetches from any HTTP server that exposes a
  `GET /trades.arrow` endpoint returning an
  [Arrow IPC](https://arrow.apache.org/) stream with:
    - Response: `Content-Type: application/vnd.apache.arrow.stream`
    - Schema (columns matched by name, order not significant):

        `ts (int64)`, `price (float64)`, `qty (float64)`, `is_sell (bool)`

    - Query parameters:
      `venue`, `market`, `symbol`, `from` (inclusive), `to` (inclusive), `limit`
    - Optional bearer-token auth: (`Authorization: Bearer <token>`)

    Self-signed TLS certificates are
    accepted. Works with **all exchanges** and has no exchange rate limits.
    Bring your own data source.

    A reference implementation is available at
    [flowsurface-server](https://github.com/flowsurface-rs/server), a
    self-contained collector that writes trades to DuckDB and serves
    them over this protocol.

- **Off**: Live trades only; no historical backfill.

##### Historical klines, open interest, and ticker metadata/stats are always fetched from exchange REST APIs.

##### Live trades, orderbook and kline updates are streamed from exchange WebSocket feeds.

## Installation

### Prebuilt beta packages

Follow the [quick-start download steps](#download). Packages contain the application binary and the assets needed by that platform.

### Build from source

Requirements:

- [Rust](https://www.rust-lang.org/tools/install); the pinned version and components are defined in [`rust-toolchain.toml`](./rust-toolchain.toml).
- [Git](https://git-scm.com/).
- Platform dependencies:
  - Debian/Ubuntu: `sudo apt install build-essential pkg-config libasound2-dev`
  - Arch Linux: `sudo pacman -S base-devel alsa-lib`
  - Fedora: `sudo dnf install gcc make alsa-lib-devel`
  - macOS: install Xcode Command Line Tools with `xcode-select --install`.
  - Windows: use a Rust MSVC toolchain; no additional project dependency is required.

Install directly from the `develop` branch:

```bash
cargo install --git https://github.com/Niketion/flowdepth.git --branch develop flowsurface
flowsurface
```

Or clone, build, and run the repository:

```bash
git clone --branch develop https://github.com/Niketion/flowdepth.git
cd flowdepth
cargo build --release --locked
cargo run --release --locked
```

The application binary and Rust package are currently still named `flowsurface` for upstream compatibility. The public project and fork are named Flowdepth; internal crate and module names intentionally retain their upstream-compatible names.

## Technical documentation

- [Adaptive volume bubbles](./docs/adaptive-volume-bubbles.md)
- [Binance iceberg/replenishment detector](./docs/binance-iceberg-detector.md)
- [Derive maker flow and GEX architecture](./docs/derive-maker-flow.md)
- [Contributing guide](./CONTRIBUTING.md)
- [Security policy](./SECURITY.md)
- [Code of Conduct](./CODE_OF_CONDUCT.md)

### Notable changes from `upstream/main`

The `develop` branch adds the order-flow, options, caching, recovery, diagnostics, saved-state, and build automation features summarized above. Windows uses a single-native-window compatibility mode to avoid an upstream multi-window redraw issue, while macOS and Linux retain native multi-window behavior.

See the complete [`upstream/main...develop` source comparison](https://github.com/flowsurface-rs/flowsurface/compare/main...Niketion:develop).

## Project relationship

Flowdepth is a fork of [Flowsurface](https://github.com/flowsurface-rs/flowsurface). It retains upstream compatibility and attribution and does not seek to obscure or replace the origin of its code. Flowdepth-specific features are developed independently, and Flowdepth releases and support are separate from official Flowsurface releases.

The Rust package and executable remain named `flowsurface` for compatibility. Changes can be reviewed directly in the [`upstream/main...develop` comparison](https://github.com/flowsurface-rs/flowsurface/compare/main...Niketion:develop).

## Credits

- [Flowsurface](https://github.com/flowsurface-rs/flowsurface) and its contributors, whose project is the foundation of this fork.
- [Kraken Desktop](https://www.kraken.com/desktop), formerly [Cryptowatch](https://blog.kraken.com/product/cryptowatch-to-sunset-kraken-pro-to-integrate-cryptowatch-features), which inspired the original project.
- [Halloy](https://github.com/squidowl/halloy), an open-source reference for foundational design and architecture.
- [iced](https://github.com/iced-rs/iced), the Rust GUI library used by the application.

## Community

Questions and discussion are welcome in this repository's [GitHub issues](https://github.com/Niketion/flowdepth/issues). The existing [Discord invite](https://discord.gg/RN2XAF7ZuR) leads to the upstream Flowsurface community; Flowdepth releases and support remain separate.

Please read the [contribution guide](./CONTRIBUTING.md) and [Code of Conduct](./CODE_OF_CONDUCT.md) before contributing.

## License

Flowdepth is distributed under the [GNU General Public License v3.0 or later](./LICENSE), consistent with its Flowsurface foundation. Contributions are shared under the same license.
