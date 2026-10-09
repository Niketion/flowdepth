use super::{
    OptionInstrument, OptionMarketPoint, OptionRight, OptionsProvider, OptionsUnderlying,
    RawOptionChainSnapshot, RawOptionContractSnapshot,
};
use crate::{UnixMs, adapter};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use thiserror::Error;

const PRODUCTION_BASE_URL: &str = "https://www.deribit.com/api/v2";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const NATIVE_GAMMA_REFRESH_MS: u64 = 60_000;
const MAX_NATIVE_GAMMA_INSTRUMENTS: usize = 128;

#[derive(Debug, Default)]
struct NativeGammaState {
    last_refresh: Option<UnixMs>,
    values: HashMap<String, (f64, UnixMs)>,
}

#[derive(Debug, Error)]
pub enum DeribitError {
    #[error("failed to build Deribit HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("Deribit HTTP request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("Deribit returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("invalid Deribit JSON response: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("Deribit JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("Deribit JSON-RPC response did not contain result")]
    MissingResult,
    #[error("Deribit returned no valid option contracts")]
    EmptySnapshot,
}

#[derive(Debug, Clone)]
pub struct DeribitOptionsClient {
    client: Client,
    base_url: String,
    native_gamma: Arc<Mutex<HashMap<OptionsUnderlying, NativeGammaState>>>,
}

impl DeribitOptionsClient {
    pub fn new(proxy: Option<&adapter::Proxy>) -> Result<Self, DeribitError> {
        Self::with_base_url(PRODUCTION_BASE_URL, proxy)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        proxy: Option<&adapter::Proxy>,
    ) -> Result<Self, DeribitError> {
        let builder = Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT);
        let client = adapter::proxy::try_apply_proxy(builder, proxy)
            .build()
            .map_err(DeribitError::Client)?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            native_gamma: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn fetch_instruments(
        &self,
        underlying: OptionsUnderlying,
    ) -> Result<Vec<OptionInstrument>, DeribitError> {
        log::info!("GEX FetchStarted kind=instruments underlying={underlying} provider=Deribit");
        let result = self
            .get::<Vec<InstrumentDto>>(
                "public/get_instruments",
                underlying,
                &[("expired", "false")],
            )
            .await;
        match result {
            Ok(items) => {
                let instruments = items
                    .into_iter()
                    .filter_map(|dto| dto.into_model(underlying))
                    .collect::<Vec<_>>();
                log::info!(
                    "GEX InstrumentsRefreshed underlying={underlying} count={}",
                    instruments.len()
                );
                Ok(instruments)
            }
            Err(error) => {
                log::warn!(
                    "GEX FetchFailed kind=instruments underlying={underlying} error={error}"
                );
                Err(error)
            }
        }
    }

    pub async fn fetch_chain(
        &self,
        underlying: OptionsUnderlying,
        instruments: &[OptionInstrument],
    ) -> Result<RawOptionChainSnapshot, DeribitError> {
        log::info!("GEX FetchStarted kind=snapshot underlying={underlying} provider=Deribit");
        let summaries = self
            .get::<Vec<BookSummaryDto>>("public/get_book_summary_by_currency", underlying, &[])
            .await
            .inspect_err(|error| {
                log::warn!("GEX FetchFailed kind=snapshot underlying={underlying} error={error}");
            })?;
        let mut snapshot = merge_snapshot(underlying, instruments, summaries, UnixMs::now())?;
        self.enrich_native_gamma(&mut snapshot).await;
        log::info!(
            "GEX SnapshotRefreshed underlying={underlying} contracts={} observed_at={}",
            snapshot.contracts.len(),
            snapshot.observed_at
        );
        log::info!("GEX FetchCompleted kind=snapshot underlying={underlying}");
        Ok(snapshot)
    }

    async fn enrich_native_gamma(&self, snapshot: &mut RawOptionChainSnapshot) {
        let now = UnixMs::now();
        let cached = {
            let Ok(mut states) = self.native_gamma.lock() else {
                return;
            };
            let state = states.entry(snapshot.underlying).or_default();
            if state
                .last_refresh
                .is_some_and(|last| now.saturating_diff(last) < NATIVE_GAMMA_REFRESH_MS)
            {
                Some(state.values.clone())
            } else {
                state.last_refresh = Some(now);
                None
            }
        };
        let values = if let Some(cached) = cached {
            cached
        } else {
            let candidates = select_native_gamma_candidates(snapshot, MAX_NATIVE_GAMMA_INSTRUMENTS);
            match self.fetch_native_gamma_batch(&candidates, now).await {
                Ok(values) => {
                    if let Ok(mut states) = self.native_gamma.lock() {
                        states.entry(snapshot.underlying).or_default().values = values.clone();
                    }
                    values
                }
                Err(error) => {
                    log::debug!(
                        "GEX NativeGammaFallback | underlying={} error={error}",
                        snapshot.underlying
                    );
                    HashMap::new()
                }
            }
        };
        let contracts = Arc::make_mut(&mut snapshot.contracts);
        for contract in contracts {
            if let Some((gamma, observed_at)) = values.get(&contract.instrument.instrument_name)
                && gamma.is_finite()
                && *gamma >= 0.0
            {
                contract.market.native_gamma = Some(*gamma);
                contract.market.native_gamma_observed_at = Some(*observed_at);
            }
        }
    }

    async fn fetch_native_gamma_batch(
        &self,
        instruments: &[String],
        now: UnixMs,
    ) -> Result<HashMap<String, (f64, UnixMs)>, DeribitError> {
        if instruments.is_empty() {
            return Ok(HashMap::new());
        }
        let requests = instruments
            .iter()
            .enumerate()
            .map(|(id, instrument_name)| NativeTickerRequest {
                jsonrpc: "2.0",
                id: id as u64,
                method: "public/ticker",
                params: NativeTickerParams { instrument_name },
            })
            .collect::<Vec<_>>();
        let response = self
            .client
            .post(&self.base_url)
            .json(&requests)
            .send()
            .await
            .map_err(DeribitError::Request)?;
        let status = response.status();
        let body = response.text().await.map_err(DeribitError::Request)?;
        if !status.is_success() {
            return Err(DeribitError::Http {
                status: status.as_u16(),
                message: body.chars().take(256).collect(),
            });
        }
        let replies: Vec<NativeTickerReply> =
            serde_json::from_str(&body).map_err(DeribitError::Decode)?;
        let mut values = HashMap::new();
        for reply in replies {
            let Some(instrument) = instruments.get(reply.id as usize) else {
                continue;
            };
            let Some(result) = reply.result else {
                continue;
            };
            let Some(gamma) = result.greeks.and_then(|greeks| greeks.gamma) else {
                continue;
            };
            let observed_at = result.timestamp.map(UnixMs::new).unwrap_or(now);
            if gamma.is_finite() && gamma >= 0.0 {
                values.insert(instrument.clone(), (gamma, observed_at));
            }
        }
        Ok(values)
    }

    async fn get<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        underlying: OptionsUnderlying,
        extra: &[(&str, &str)],
    ) -> Result<T, DeribitError> {
        let mut request = self
            .client
            .get(format!("{}/{}", self.base_url, method))
            .query(&[
                (
                    "currency",
                    if underlying == OptionsUnderlying::Sol {
                        "USDC"
                    } else {
                        underlying.as_str()
                    },
                ),
                ("kind", "option"),
            ]);
        if !extra.is_empty() {
            request = request.query(extra);
        }
        let response = request.send().await.map_err(DeribitError::Request)?;
        let status = response.status();
        let body = response.text().await.map_err(DeribitError::Request)?;
        if !status.is_success() {
            return Err(DeribitError::Http {
                status: status.as_u16(),
                message: body.chars().take(256).collect(),
            });
        }
        parse_rpc(&body)
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    jsonrpc: Option<String>,
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

fn parse_rpc<T: for<'de> Deserialize<'de>>(body: &str) -> Result<T, DeribitError> {
    let envelope: JsonRpcResponse<T> = serde_json::from_str(body).map_err(DeribitError::Decode)?;
    if envelope.jsonrpc.as_deref() != Some("2.0") {
        return Err(DeribitError::MissingResult);
    }
    if let Some(error) = envelope.error {
        return Err(DeribitError::Rpc {
            code: error.code,
            message: error.message,
        });
    }
    envelope.result.ok_or(DeribitError::MissingResult)
}

#[derive(Debug, Clone, Deserialize)]
struct InstrumentDto {
    instrument_name: String,
    #[serde(default)]
    base_currency: Option<String>,
    expiration_timestamp: u64,
    strike: f64,
    option_type: String,
    contract_size: f64,
    #[serde(default)]
    is_active: bool,
}

impl InstrumentDto {
    fn into_model(self, underlying: OptionsUnderlying) -> Option<OptionInstrument> {
        // The USDC endpoint includes several assets. Accept only the requested
        // chain, including when older cached metadata omits base_currency.
        let prefix = match underlying {
            OptionsUnderlying::Btc => "BTC-",
            OptionsUnderlying::Eth => "ETH-",
            OptionsUnderlying::Sol => "SOL_USDC-",
            OptionsUnderlying::Gld | OptionsUnderlying::Ndx => return None,
        };
        if !self.instrument_name.starts_with(prefix)
            || self
                .base_currency
                .as_deref()
                .is_some_and(|base| base != underlying.as_str())
        {
            return None;
        }
        let right = match self.option_type.as_str() {
            "call" => OptionRight::Call,
            "put" => OptionRight::Put,
            _ => return None,
        };
        (self.is_active
            && self.strike.is_finite()
            && self.strike > 0.0
            && self.contract_size.is_finite()
            && self.contract_size > 0.0)
            .then_some(OptionInstrument {
                instrument_name: self.instrument_name,
                underlying,
                expiration_timestamp: UnixMs::new(self.expiration_timestamp),
                strike: self.strike,
                right,
                // Deribit reports option OI in underlying coins, not lots.
                // One model exposure unit is therefore one coin. In particular,
                // the SOL trading-lot multiplier must not multiply OI or Derive
                // trade amounts a second time in the downstream GEX formulas.
                contract_size: 1.0,
            })
    }
}

#[derive(Debug, Deserialize)]
struct BookSummaryDto {
    instrument_name: String,
    open_interest: Option<f64>,
    mark_iv: Option<f64>,
    underlying_price: Option<f64>,
    interest_rate: Option<f64>,
    creation_timestamp: Option<u64>,
}

#[derive(Debug, Serialize)]
struct NativeTickerRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: NativeTickerParams<'a>,
}

#[derive(Debug, Serialize)]
struct NativeTickerParams<'a> {
    instrument_name: &'a str,
}

#[derive(Debug, Deserialize)]
struct NativeTickerReply {
    id: u64,
    result: Option<NativeTickerResult>,
}

#[derive(Debug, Deserialize)]
struct NativeTickerResult {
    timestamp: Option<u64>,
    greeks: Option<NativeGreeks>,
}

#[derive(Debug, Deserialize)]
struct NativeGreeks {
    gamma: Option<f64>,
}

fn select_native_gamma_candidates(
    snapshot: &RawOptionChainSnapshot,
    maximum: usize,
) -> Vec<String> {
    let lower = snapshot.source_spot * 0.85;
    let upper = snapshot.source_spot * 1.15;
    let mut candidates = snapshot
        .contracts
        .iter()
        .filter(|contract| {
            contract.instrument.strike >= lower && contract.instrument.strike <= upper
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.market
            .open_interest_underlying
            .total_cmp(&a.market.open_interest_underlying)
            .then_with(|| {
                a.instrument
                    .instrument_name
                    .cmp(&b.instrument.instrument_name)
            })
    });
    candidates
        .into_iter()
        .take(maximum.clamp(32, 256))
        .map(|contract| contract.instrument.instrument_name.clone())
        .collect()
}

fn merge_snapshot(
    underlying: OptionsUnderlying,
    instruments: &[OptionInstrument],
    summaries: Vec<BookSummaryDto>,
    now: UnixMs,
) -> Result<RawOptionChainSnapshot, DeribitError> {
    let metadata = instruments
        .iter()
        .map(|instrument| (instrument.instrument_name.as_str(), instrument))
        .collect::<HashMap<_, _>>();
    let mut contracts = Vec::new();
    let mut missing_metadata = 0usize;
    let mut invalid = 0usize;

    for summary in summaries {
        let Some(instrument) = metadata.get(summary.instrument_name.as_str()) else {
            missing_metadata += 1;
            continue;
        };
        let Some((oi, iv, spot, rate)) = summary
            .open_interest
            .zip(summary.mark_iv)
            .zip(summary.underlying_price)
            .zip(summary.interest_rate)
            .map(|(((oi, iv), spot), rate)| (oi, iv, spot, rate))
        else {
            invalid += 1;
            continue;
        };
        if instrument.expiration_timestamp <= now
            || ![oi, iv, spot, rate].iter().all(|value| value.is_finite())
            || oi < 0.0
            || iv <= 0.0
            || spot <= 0.0
        {
            invalid += 1;
            continue;
        }
        contracts.push(RawOptionContractSnapshot {
            instrument: (*instrument).clone(),
            market: OptionMarketPoint {
                instrument_name: summary.instrument_name,
                open_interest_underlying: oi,
                mark_iv_percent: iv,
                underlying_price: spot,
                interest_rate: rate,
                observed_at: UnixMs::new(summary.creation_timestamp.unwrap_or(now.as_u64())),
                native_gamma: None,
                native_gamma_observed_at: None,
            },
        });
    }
    if missing_metadata > 0 || invalid > 0 {
        log::debug!(
            "GEX SnapshotMerge underlying={underlying} missing_metadata={missing_metadata} invalid={invalid}"
        );
    }
    if contracts.is_empty() {
        return Err(DeribitError::EmptySnapshot);
    }
    let source_spot = contracts
        .iter()
        .map(|contract| contract.market.underlying_price)
        .sum::<f64>()
        / contracts.len() as f64;
    Ok(RawOptionChainSnapshot {
        provider: OptionsProvider::Deribit,
        underlying,
        source_spot,
        contracts: contracts.into(),
        observed_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const INSTRUMENTS: &str = r#"{"jsonrpc":"2.0","result":[{"instrument_name":"BTC-30JUN30-100000-C","expiration_timestamp":1909008000000,"strike":100000.0,"option_type":"call","contract_size":1.0,"is_active":true}]}"#;
    const SUMMARIES: &str = r#"{"jsonrpc":"2.0","result":[{"instrument_name":"BTC-30JUN30-100000-C","open_interest":12.5,"mark_iv":55.0,"underlying_price":101000.0,"interest_rate":0.01,"creation_timestamp":1800000000000},{"instrument_name":"UNKNOWN","open_interest":1.0,"mark_iv":50.0,"underlying_price":101000.0,"interest_rate":0.0}]}"#;
    const SOL_INSTRUMENTS: &str = r#"{"jsonrpc":"2.0","result":[{"instrument_name":"SOL_USDC-30JUN30-100-C","base_currency":"SOL","expiration_timestamp":1909008000000,"strike":100.0,"option_type":"call","contract_size":10.0,"is_active":true},{"instrument_name":"ETH_USDC-30JUN30-3000-C","base_currency":"ETH","expiration_timestamp":1909008000000,"strike":3000.0,"option_type":"call","contract_size":1.0,"is_active":true},{"instrument_name":"SOL_USDC-30JUN30-100-P","base_currency":"ETH","expiration_timestamp":1909008000000,"strike":100.0,"option_type":"put","contract_size":10.0,"is_active":true}]}"#;
    const SOL_SUMMARIES: &str = r#"{"jsonrpc":"2.0","result":[{"instrument_name":"SOL_USDC-30JUN30-100-C","open_interest":250.0,"mark_iv":50.0,"underlying_price":110.0,"interest_rate":0.0},{"instrument_name":"ETH_USDC-30JUN30-3000-C","open_interest":1.0,"mark_iv":50.0,"underlying_price":3000.0,"interest_rate":0.0}]}"#;

    #[tokio::test]
    #[ignore = "contacts the public Deribit API; run explicitly for a live SOL smoke test"]
    async fn sol_live_chain_contains_only_sol_options() {
        let client = DeribitOptionsClient::new(None).expect("client");
        let instruments = client
            .fetch_instruments(OptionsUnderlying::Sol)
            .await
            .expect("live SOL instruments");
        assert!(!instruments.is_empty());
        assert!(
            instruments
                .iter()
                .all(|instrument| instrument.underlying == OptionsUnderlying::Sol
                    && instrument.instrument_name.starts_with("SOL_USDC-")
                    && instrument.contract_size == 1.0)
        );
        let chain = client
            .fetch_chain(OptionsUnderlying::Sol, &instruments)
            .await
            .expect("live SOL chain");
        assert!(chain.source_spot.is_finite() && chain.source_spot > 0.0);
        assert!(!chain.contracts.is_empty());
        assert!(
            chain
                .contracts
                .iter()
                .all(
                    |contract| contract.instrument.underlying == OptionsUnderlying::Sol
                        && contract.market.open_interest_underlying.is_finite()
                )
        );
        assert!(
            chain
                .contracts
                .iter()
                .any(|contract| contract.market.open_interest_underlying > 0.0)
        );
        println!(
            "Live SOL chain: {} instruments, {} valid contracts, spot {:.4}",
            instruments.len(),
            chain.contracts.len(),
            chain.source_spot
        );
    }

    #[tokio::test]
    async fn sol_chain_uses_usdc_filters_other_assets_and_normalizes_exposure_units() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let native = format!(
                r#"[{{"id":0,"result":{{"timestamp":{},"greeks":{{"gamma":0.01}}}}}}]"#,
                UnixMs::now().as_u64()
            );
            for (endpoint, body) in [
                ("GET /api/v2/public/get_instruments?", SOL_INSTRUMENTS),
                (
                    "GET /api/v2/public/get_book_summary_by_currency?",
                    SOL_SUMMARIES,
                ),
                ("POST /api/v2", native.as_str()),
            ] {
                let (mut stream, _) = listener.accept().expect("connection");
                let mut buffer = [0u8; 4096];
                let count = stream.read(&mut buffer).expect("request");
                let request = String::from_utf8_lossy(&buffer[..count]);
                assert!(request.starts_with(endpoint), "{request}");
                if endpoint.starts_with("GET") {
                    assert!(request.contains("currency=USDC"));
                    assert!(request.contains("kind=option"));
                    assert!(!request.contains("currency=SOL"));
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("response");
            }
        });
        let client = DeribitOptionsClient::with_base_url(format!("http://{address}/api/v2"), None)
            .expect("client");
        let instruments = client
            .fetch_instruments(OptionsUnderlying::Sol)
            .await
            .expect("SOL instruments");
        assert_eq!(instruments.len(), 1);
        assert_eq!(instruments[0].underlying, OptionsUnderlying::Sol);
        let chain = client
            .fetch_chain(OptionsUnderlying::Sol, &instruments)
            .await
            .expect("SOL chain");
        assert_eq!(chain.underlying, OptionsUnderlying::Sol);
        assert_eq!(chain.source_spot, 110.0);
        assert_eq!(chain.contracts.len(), 1);
        assert_eq!(chain.contracts[0].instrument.contract_size, 1.0);
        assert_eq!(chain.contracts[0].market.open_interest_underlying, 250.0);
        assert_eq!(chain.contracts[0].market.native_gamma, Some(0.01));
        server.join().expect("server");
    }

    #[test]
    fn parses_instruments_and_nullable_summary_fields() {
        let dtos: Vec<InstrumentDto> = parse_rpc(INSTRUMENTS).expect("fixture");
        let model = dtos[0]
            .to_owned()
            .into_model(OptionsUnderlying::Btc)
            .expect("valid");
        assert_eq!(model.right, OptionRight::Call);
        let nullable = r#"{"jsonrpc":"2.0","result":[{"instrument_name":"x","open_interest":null,"mark_iv":null,"underlying_price":null,"interest_rate":null,"creation_timestamp":null}]}"#;
        let values: Vec<BookSummaryDto> = parse_rpc(nullable).expect("nullable");
        assert!(values[0].open_interest.is_none());
    }

    #[test]
    fn validates_json_rpc_error_and_missing_result() {
        let error = parse_rpc::<Vec<InstrumentDto>>(
            r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"bad"}}"#,
        );
        assert!(matches!(error, Err(DeribitError::Rpc { code: -1, .. })));
        assert!(matches!(
            parse_rpc::<Vec<InstrumentDto>>(r#"{"jsonrpc":"2.0"}"#),
            Err(DeribitError::MissingResult)
        ));
    }

    #[test]
    fn merges_by_instrument_name_and_skips_unknown_or_missing() {
        let instruments = parse_rpc::<Vec<InstrumentDto>>(INSTRUMENTS)
            .expect("fixture")
            .into_iter()
            .filter_map(|dto| dto.into_model(OptionsUnderlying::Btc))
            .collect::<Vec<_>>();
        let summaries = parse_rpc::<Vec<BookSummaryDto>>(SUMMARIES).expect("fixture");
        let snapshot = merge_snapshot(
            OptionsUnderlying::Btc,
            &instruments,
            summaries,
            UnixMs::new(1_800_000_000_000),
        )
        .expect("snapshot");
        assert_eq!(snapshot.contracts.len(), 1);
        assert_eq!(snapshot.contracts[0].market.open_interest_underlying, 12.5);
    }

    #[test]
    fn native_gamma_candidates_are_bounded_near_spot_and_oi_ordered() {
        let instruments = parse_rpc::<Vec<InstrumentDto>>(INSTRUMENTS)
            .expect("fixture")
            .into_iter()
            .filter_map(|dto| dto.into_model(OptionsUnderlying::Btc))
            .collect::<Vec<_>>();
        let summaries = parse_rpc::<Vec<BookSummaryDto>>(SUMMARIES).expect("fixture");
        let base = merge_snapshot(
            OptionsUnderlying::Btc,
            &instruments,
            summaries,
            UnixMs::new(1_800_000_000_000),
        )
        .expect("snapshot");
        let template = base.contracts[0].clone();
        let mut contracts = (0..300)
            .map(|index| {
                let mut contract = template.clone();
                contract.instrument.instrument_name = format!("BTC-CANDIDATE-{index:03}");
                contract.instrument.strike = 90_000.0 + f64::from(index);
                contract.market.open_interest_underlying = f64::from(index);
                contract
            })
            .collect::<Vec<_>>();
        let mut outside = template;
        outside.instrument.instrument_name = "BTC-OUTSIDE".to_owned();
        outside.instrument.strike = 200_000.0;
        outside.market.open_interest_underlying = f64::MAX;
        contracts.push(outside);
        let snapshot = RawOptionChainSnapshot {
            contracts: contracts.into(),
            ..base
        };
        let selected = select_native_gamma_candidates(&snapshot, 128);
        assert_eq!(selected.len(), 128);
        assert_eq!(
            selected.first().map(String::as_str),
            Some("BTC-CANDIDATE-299")
        );
        assert!(!selected.iter().any(|name| name == "BTC-OUTSIDE"));
    }

    #[tokio::test]
    async fn injectable_base_url_uses_batch_instruments_endpoint() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0u8; 4096];
            let count = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /api/v2/public/get_instruments?"));
            assert!(request.contains("currency=BTC"));
            assert!(request.contains("kind=option"));
            assert!(request.contains("expired=false"));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                INSTRUMENTS.len(),
                INSTRUMENTS
            );
            stream.write_all(response.as_bytes()).expect("response");
        });
        let client = DeribitOptionsClient::with_base_url(format!("http://{address}/api/v2"), None)
            .expect("client");
        let instruments = client
            .fetch_instruments(OptionsUnderlying::Btc)
            .await
            .expect("instruments");
        assert_eq!(instruments.len(), 1);
        server.join().expect("server");
    }
}
