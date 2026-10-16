# Windows build and saved-configuration verification

The aligned develop retains the original venues: Binance, Bybit, Hyperliquid, OKX and MEXC. The `Exchange` and `Venue` variants match the pre-alignment develop, and options-provider definitions remain unchanged. Paradex and providers absent from that develop are excluded.

## Isolated checks

`src/config_verification.rs` provides two opt-in tests. Fixtures must be copies of local settings; private settings are not committed. Set `FLOWSURFACE_CONFIG_FIXTURES` to a directory containing `*.fixture.json`, and set `FLOWSURFACE_DATA_PATH` to a disposable test-data directory. Each fixture is loaded from a separate temporary subdirectory because normal recovery writes backups and prunes older files. The fixture inputs remain unchanged.

```powershell
cargo test --workspace --all-targets --all-features --locked config_verification:: -- --ignored --nocapture --test-threads=1
```

The inventory test checks layout counts, chart types, indicators, drawings, supported market streams, serialized state round trips and reconstruction of runtime dashboards. Unsupported provider streams are expected to be excluded by develop's existing recovery; their chart shells and settings remain. Old window positions outside the current screen may be recentered. Other unexpected recovery warnings fail the check.

The GPU test renders the five current layouts and a second configuration enabling RSI, ATR, MACD, ADX and TPO. It uses synthetic OHLCV bars to exercise chart and indicator drawing. These renders do not establish live market-data availability. The renderer checks output dimensions and visible content and writes RGBA images for inspection. This test expects copied fixtures named `saved-state.json.fixture.json` and `new-indicators.fixture.json`.

## Executable smoke checks

Build with `cargo build --release --locked --bin flowsurface`. Run the executable with `FLOWSURFACE_DATA_PATH` and `FLOWSURFACE_LOG_DIR` pointing to isolated copies, once for each active layout. Check that the process stays alive, Windows reports it responsive, the window identifies the selected layout, and logs show WebSocket connections without a panic or runtime error. Stop only the test process after collecting evidence.

A layout containing an excluded provider loads through the existing recovery path. Its unsupported streams are removed from the copied state, while supported streams and chart settings are retained. This is expected compatibility behavior, not evidence that an excluded provider works.

During local smoke checks, Derive returned HTTP 503, and Binance rejected a request for old SOL aggregate trades with code -4166 (recent-two-days limit). Those feeds were not fully verified. Desktop interaction through Computer Use was unavailable; verification uses process/log checks and application-level GPU rendering.

Release artifacts must contain the executable, public assets and licenses only. Keep copied user configurations, logs and data caches outside the distributable. Confirm original settings hashes after verification.
