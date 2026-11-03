# Derive Maker Flow and GEX architecture

## Data-source responsibilities

The GEX integration keeps structural exposure and observed flow separate:

- **Deribit is the structural source.** Its option chain drives Net GEX, absolute GEX, Gamma Flip, Call/Put Walls, the current profile, persistent zones, projections, and GEX Monitor comparisons.
- **Derive is an observed-flow source.** Only settled option trades where Derive reports the participant as maker are retained.
- **The datasets are never summed.** Derive flow does not alter Deribit GEX, walls, flip, zones, projection, or monitor data, and Derive is not exposed as a chartable exchange adapter.

The Derive client uses REST JSON-RPC at `https://api.derive.xyz/v3`:

- `public/get_all_instruments`
- `public/get_trade_history`

Instrument and trade history requests are paginated at 1,000 records. Only active, non-expired BTC and ETH options are accepted. Trade history is restricted to settled maker trades and deduplicated by `trade_id`. Wallets, subaccounts, fees, realized PnL, and transaction hashes are neither retained nor displayed.

## Exact contract matching

Derive and Deribit contracts share a normalized key containing:

- underlying;
- UTC expiry day;
- strike rounded as `round(strike * 100)`;
- call/put right.

A Derive trade matches a Deribit contract only when the complete key is identical and the expiry timestamps differ by no more than 12 hours. There is no nearest-strike or nearest-expiry fallback, and direction is never transferred to a different strike or expiry.

## Maker gamma flow

Each exact match uses the current gamma of the corresponding Deribit contract:

```text
absolute_gamma_per_contract_1pct = abs(gamma) * contract_size * spot^2 * 0.01
trade_gamma = absolute_gamma_per_contract_1pct * derive_trade_amount
```

A maker buy is positive/long gamma and a maker sell is negative/short gamma for both calls and puts.

For each 5-minute, 30-minute, and 2-hour window:

```text
signed = sum(signed trade_gamma)
gross = sum(abs(trade_gamma))
imbalance = signed / gross
```

Direction is Long Gamma at or above `+0.20`, Short Gamma at or below `-0.20`, and Balanced otherwise. A window with no usable data is Unavailable.

Matched share is the absolute Deribit GEX of contracts with a matched Derive trade divided by total absolute Deribit GEX. Each Deribit contract is counted once in the numerator.

Quality is:

- **High:** at least 5 trades, matched share at least 10%, and absolute imbalance at least 35%;
- **Medium:** at least 3 trades, matched share at least 3%, and absolute imbalance at least 20%;
- **Low:** every other case.

## OI-proxy agreement

The agreement gauge compares the 30-minute Derive flow with the currently selected Deribit expiry, minimum-OI, and minimum-GEX scope.

- **Agree:** Derive quality is Medium or High and its sign matches the Deribit OI proxy.
- **Diverge:** the qualified Derive flow and Deribit OI proxy have opposite signs.
- **Insufficient:** quality is Low, either side is balanced, or the selected scope has insufficient exact matches.

The Derive summary count is global, while the gauge count is expiry-aligned. They can therefore differ. For example, `Next 1 day` excludes recent trades whose matched contracts expire beyond the next rolling 24 hours.

Available filters are Next expiry, Next 1 day, Next 2 days, Next 3 days, Next 7 days, Next 30 days, and All expiries.

## Refresh, offline behavior, and cache

Derive state and backoff are independent from Deribit and GEX Monitor:

- instruments refresh every 10 minutes;
- trades refresh every 5 seconds;
- initial history covers 2 hours;
- incremental fetches overlap by 10 seconds;
- in-memory and persistent retention is 24 hours, plus a 10-minute cache margin;
- persistent storage is capped at 50,000 trades per underlying.

Offline mode performs no Derive requests and preserves valid loaded data. Reconnect forces instrument and trade refresh without clearing the cache. On startup, cached data is stale until refreshed and the latest cached timestamp becomes the watermark.

## Strike-weighted GEX zones

Historical GEX zones render one narrow band per strike instead of filling the complete cluster envelope. Local body alpha and core strength use each strike's normalized GEX; persistence controls temporal visibility without promoting weak strikes to peak strength.

The zone envelope remains available for identity tracking and persistence. Rendering, projection, hover hit testing, and tooltips operate on individual bands, and spaces between bands remain unfilled.

## Binance raw-trade data quality

The Binance USDⓈ-M public `@trade` feed can emit non-execution markers with this observed signature:

```json
{"p":"0","q":"0","X":"NA","st":1}
```

These markers are logged at debug level and discarded in the Binance adapter before reaching candlestick, footprint, CVD, volume-bubble, or order-flow consumers. Other malformed or non-positive trade messages remain warnings and include raw public values, normalization results, symbol, market, trade ID, timing, and payload for diagnosis.
