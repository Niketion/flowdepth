use super::OptionsUnderlying;
use crate::adapter;
use chrono::DateTime;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

const PRODUCTION_BASE_URL: &str = "https://gexmonitor.com";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GexProxyHistoryPoint {
    pub observed_at: i64,
    pub source_spot: f64,
    pub total_gex: f64,
    pub flip_level: Option<f64>,
    pub call_wall: Option<f64>,
    pub put_wall: Option<f64>,
    pub positive_level_1: Option<f64>,
    pub positive_level_2: Option<f64>,
    pub negative_level_1: Option<f64>,
    pub negative_level_2: Option<f64>,
}

impl GexProxyHistoryPoint {
    pub fn is_semantically_valid(&self) -> bool {
        self.observed_at > 0
            && self.source_spot.is_finite()
            && self.source_spot > 0.0
            && self.total_gex.is_finite()
            && [
                self.flip_level,
                self.call_wall,
                self.put_wall,
                self.positive_level_1,
                self.positive_level_2,
                self.negative_level_1,
                self.negative_level_2,
            ]
            .into_iter()
            .flatten()
            .all(|value| value.is_finite() && value > 0.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GexProxyHistoryResponse {
    pub points: Vec<GexProxyHistoryPoint>,
    pub stale: bool,
}

#[derive(Debug, Error)]
pub enum GexMonitorError {
    #[error("failed to build GEX Monitor HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("GEX Monitor HTTP request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("GEX Monitor returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("invalid GEX Monitor JSON response: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("GEX Monitor availability is {0}")]
    Unavailable(String),
    #[error("GEX Monitor history contained no valid records")]
    Empty,
}

#[derive(Debug, Clone)]
pub struct GexMonitorClient {
    client: Client,
    base_url: String,
}

impl GexMonitorClient {
    pub fn new(proxy: Option<&adapter::Proxy>) -> Result<Self, GexMonitorError> {
        Self::with_base_url(PRODUCTION_BASE_URL, proxy)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        proxy: Option<&adapter::Proxy>,
    ) -> Result<Self, GexMonitorError> {
        let builder = Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT);
        let client = adapter::proxy::try_apply_proxy(builder, proxy)
            .build()
            .map_err(GexMonitorError::Client)?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        })
    }

    pub fn history_url(&self, underlying: OptionsUnderlying) -> String {
        build_history_url(&self.base_url, underlying)
    }

    pub async fn fetch_history(
        &self,
        underlying: OptionsUnderlying,
    ) -> Result<GexProxyHistoryResponse, GexMonitorError> {
        let url = self.history_url(underlying);
        log::info!("GEX Monitor FetchStarted underlying={underlying} url={url}");
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(GexMonitorError::Request)?;
        let status = response.status();
        let body = response.text().await.map_err(GexMonitorError::Request)?;
        if !status.is_success() {
            return Err(GexMonitorError::Http {
                status: status.as_u16(),
                message: body.chars().take(256).collect(),
            });
        }
        let parsed = parse_history_response(&body)?;
        log::info!(
            "GEX Monitor FetchCompleted underlying={underlying} records={} stale={}",
            parsed.points.len(),
            parsed.stale
        );
        Ok(parsed)
    }
}

pub fn build_history_url(base_url: &str, underlying: OptionsUnderlying) -> String {
    let asset = underlying.as_str();
    format!(
        "{}/api/gex-history?asset={asset}&range=24h&format=flat",
        base_url.trim_end_matches('/')
    )
}

#[derive(Debug, Deserialize)]
struct HistoryEnvelope {
    availability: String,
    #[serde(default)]
    stale: bool,
    #[serde(alias = "history", alias = "records", alias = "items")]
    data: HistoryData,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HistoryData {
    Flat(Vec<HistoryPointDto>),
    Wrapped { data: Vec<HistoryPointDto> },
}

impl HistoryData {
    fn into_points(self) -> Vec<HistoryPointDto> {
        match self {
            Self::Flat(points) | Self::Wrapped { data: points } => points,
        }
    }
}

#[derive(Debug, Deserialize)]
struct HistoryPointDto {
    timestamp: String,
    spot_price: f64,
    total_gex: f64,
    #[serde(default)]
    flip_level: Option<f64>,
    #[serde(default)]
    flip_point: Option<f64>,
    #[serde(default)]
    call_wall: Option<f64>,
    #[serde(default)]
    put_wall: Option<f64>,
    #[serde(default)]
    p1: Option<f64>,
    #[serde(default)]
    p2: Option<f64>,
    #[serde(default)]
    n1: Option<f64>,
    #[serde(default)]
    n2: Option<f64>,
}

pub fn parse_history_response(body: &str) -> Result<GexProxyHistoryResponse, GexMonitorError> {
    let envelope: HistoryEnvelope = serde_json::from_str(body).map_err(GexMonitorError::Decode)?;
    if envelope.availability != "ready" {
        return Err(GexMonitorError::Unavailable(envelope.availability));
    }
    let mut points = envelope
        .data
        .into_points()
        .into_iter()
        .filter_map(|value| {
            let observed_at = DateTime::parse_from_rfc3339(&value.timestamp)
                .ok()?
                .timestamp_millis();
            let point = GexProxyHistoryPoint {
                observed_at,
                source_spot: value.spot_price,
                total_gex: value.total_gex,
                flip_level: value.flip_level.or(value.flip_point),
                call_wall: value.call_wall,
                put_wall: value.put_wall,
                positive_level_1: value.p1,
                positive_level_2: value.p2,
                negative_level_1: value.n1,
                negative_level_2: value.n2,
            };
            point.is_semantically_valid().then_some(point)
        })
        .collect::<Vec<_>>();
    points.sort_by_key(|point| point.observed_at);
    points.dedup_by_key(|point| point.observed_at);
    if points.is_empty() {
        return Err(GexMonitorError::Empty);
    }
    Ok(GexProxyHistoryResponse {
        points,
        stale: envelope.stale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/gex_monitor_history_flat.json");

    #[test]
    fn parses_flat_fixture_sorts_deduplicates_and_normalizes_offset() {
        let parsed = parse_history_response(FIXTURE).unwrap();
        assert_eq!(parsed.points.len(), 2);
        assert_eq!(parsed.points[0].observed_at, 1_785_139_200_000);
        assert_eq!(parsed.points[1].observed_at, 1_785_139_500_000);
        assert_eq!(parsed.points[1].flip_level, Some(117_000.0));
    }

    #[test]
    fn builds_supported_crypto_urls() {
        assert_eq!(
            build_history_url("https://example.test/", OptionsUnderlying::Btc),
            "https://example.test/api/gex-history?asset=BTC&range=24h&format=flat"
        );
        assert_eq!(
            build_history_url("https://example.test", OptionsUnderlying::Eth),
            "https://example.test/api/gex-history?asset=ETH&range=24h&format=flat"
        );
        assert_eq!(
            build_history_url("https://example.test", OptionsUnderlying::Sol),
            "https://example.test/api/gex-history?asset=SOL&range=24h&format=flat"
        );
    }

    #[test]
    fn rejects_non_finite_and_invalid_price_records() {
        for body in [
            r#"{"availability":"ready","data":[{"timestamp":"2026-07-27T08:00:00Z","spot_price":1.0,"total_gex":1e999}]}"#,
            r#"{"availability":"ready","data":[{"timestamp":"2026-07-27T08:00:00Z","spot_price":1.0,"total_gex":2.0,"p1":-1.0}]}"#,
        ] {
            assert!(parse_history_response(body).is_err());
        }
    }
}
