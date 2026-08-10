use super::{OptionsProvider, OptionsUnderlying};
use crate::{UnixMs, adapter};
use chrono::{NaiveDate, Utc};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;

const PRODUCTION_BASE_URL: &str = "https://quantwheel.com/api/tools/gex";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantWheelConfig {
    pub expiration: NaiveDate,
    pub delta_range: String,
    pub formula: String,
}

impl Default for QuantWheelConfig {
    fn default() -> Self {
        Self {
            expiration: Utc::now().date_naive(),
            delta_range: "0.97".to_owned(),
            formula: "nominal".to_owned(),
        }
    }
}

#[derive(Debug, Error)]
pub enum QuantWheelError {
    #[error("failed to build QuantWheel HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("QuantWheel HTTP request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("QuantWheel returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("invalid QuantWheel JSON response: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("QuantWheel returned no GEX levels")]
    EmptySnapshot,
    #[error("QuantWheel returned invalid {field}: {value}")]
    InvalidNumber { field: &'static str, value: f64 },
}

#[derive(Debug, Clone)]
pub struct QuantWheelGexClient {
    client: Client,
    base_url: String,
    config: QuantWheelConfig,
}

impl QuantWheelGexClient {
    pub fn new(proxy: Option<&adapter::Proxy>) -> Result<Self, QuantWheelError> {
        Self::with_base_url(PRODUCTION_BASE_URL, QuantWheelConfig::default(), proxy)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        config: QuantWheelConfig,
        proxy: Option<&adapter::Proxy>,
    ) -> Result<Self, QuantWheelError> {
        let builder = Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT);
        let client = adapter::proxy::try_apply_proxy(builder, proxy)
            .build()
            .map_err(QuantWheelError::Client)?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            config,
        })
    }

    pub async fn fetch_gex(&self) -> Result<QuantWheelGexSnapshot, QuantWheelError> {
        log::info!("GEX FetchStarted kind=snapshot underlying=GLD provider=QuantWheel");
        let response = self
            .client
            .get(&self.base_url)
            .query(&[
                ("ticker", "GLD"),
                ("expirations", &self.config.expiration.to_string()),
                ("deltaRange", self.config.delta_range.as_str()),
                ("formula", self.config.formula.as_str()),
            ])
            .send()
            .await
            .map_err(QuantWheelError::Request)?;
        let status = response.status();
        let body = response.text().await.map_err(QuantWheelError::Request)?;
        if !status.is_success() {
            return Err(QuantWheelError::Http {
                status: status.as_u16(),
                message: body.chars().take(256).collect(),
            });
        }
        let dto: QuantWheelResponseDto =
            serde_json::from_str(&body).map_err(QuantWheelError::Decode)?;
        let snapshot = dto.validate(UnixMs::now())?;
        log::info!(
            "GEX SnapshotRefreshed underlying=GLD provider=QuantWheel levels={} observed_at={}",
            snapshot.levels.len(),
            snapshot.observed_at
        );
        Ok(snapshot)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuantWheelGexLevel {
    pub strike: f64,
    pub call_gex: f64,
    pub put_gex: f64,
    pub net_gex: f64,
    pub call_open_interest: f64,
    pub put_open_interest: f64,
    pub cumulative_gex: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuantWheelWall {
    pub strike: f64,
    pub gex: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuantWheelGexSnapshot {
    pub provider: OptionsProvider,
    pub underlying: OptionsUnderlying,
    pub stock_price: f64,
    pub total_gex: f64,
    pub call_wall: Option<QuantWheelWall>,
    pub put_wall: Option<QuantWheelWall>,
    pub gamma_inflection: Option<f64>,
    pub levels: Vec<QuantWheelGexLevel>,
    pub observed_at: UnixMs,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuantWheelResponseDto {
    data: Vec<QuantWheelLevelDto>,
    #[serde(rename = "totalGEX")]
    total_gex: f64,
    call_wall: Option<QuantWheelWallDto>,
    put_wall: Option<QuantWheelWallDto>,
    gamma_inflection: Option<f64>,
    stock_price: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuantWheelLevelDto {
    strike: f64,
    #[serde(rename = "callGEX")]
    call_gex: f64,
    #[serde(rename = "putGEX")]
    put_gex: f64,
    #[serde(rename = "netGEX")]
    net_gex: f64,
    #[serde(rename = "callOI")]
    call_oi: f64,
    #[serde(rename = "putOI")]
    put_oi: f64,
    #[serde(rename = "cumulativeGEX")]
    cumulative_gex: f64,
}

#[derive(Debug, Deserialize)]
struct QuantWheelWallDto {
    strike: f64,
    gex: f64,
}

impl QuantWheelResponseDto {
    fn validate(self, observed_at: UnixMs) -> Result<QuantWheelGexSnapshot, QuantWheelError> {
        positive("stockPrice", self.stock_price)?;
        finite("totalGEX", self.total_gex)?;
        if self.data.is_empty() {
            return Err(QuantWheelError::EmptySnapshot);
        }
        let levels = self
            .data
            .into_iter()
            .map(QuantWheelLevelDto::validate)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QuantWheelGexSnapshot {
            provider: OptionsProvider::QuantWheel,
            underlying: OptionsUnderlying::Gld,
            stock_price: self.stock_price,
            total_gex: self.total_gex,
            call_wall: self
                .call_wall
                .map(QuantWheelWallDto::validate)
                .transpose()?,
            put_wall: self.put_wall.map(QuantWheelWallDto::validate).transpose()?,
            gamma_inflection: self
                .gamma_inflection
                .map(|value| positive("gammaInflection", value).map(|_| value))
                .transpose()?,
            levels,
            observed_at,
        })
    }
}

impl QuantWheelLevelDto {
    fn validate(self) -> Result<QuantWheelGexLevel, QuantWheelError> {
        positive("strike", self.strike)?;
        for (field, value) in [
            ("callGEX", self.call_gex),
            ("putGEX", self.put_gex),
            ("netGEX", self.net_gex),
            ("callOI", self.call_oi),
            ("putOI", self.put_oi),
            ("cumulativeGEX", self.cumulative_gex),
        ] {
            finite(field, value)?;
        }
        Ok(QuantWheelGexLevel {
            strike: self.strike,
            call_gex: self.call_gex,
            put_gex: self.put_gex,
            net_gex: self.net_gex,
            call_open_interest: self.call_oi,
            put_open_interest: self.put_oi,
            cumulative_gex: self.cumulative_gex,
        })
    }
}

impl QuantWheelWallDto {
    fn validate(self) -> Result<QuantWheelWall, QuantWheelError> {
        positive("wall.strike", self.strike)?;
        finite("wall.gex", self.gex)?;
        Ok(QuantWheelWall {
            strike: self.strike,
            gex: self.gex,
        })
    }
}

fn finite(field: &'static str, value: f64) -> Result<(), QuantWheelError> {
    value
        .is_finite()
        .then_some(())
        .ok_or(QuantWheelError::InvalidNumber { field, value })
}

fn positive(field: &'static str, value: f64) -> Result<(), QuantWheelError> {
    (value.is_finite() && value > 0.0)
        .then_some(())
        .ok_or(QuantWheelError::InvalidNumber { field, value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const RESPONSE: &str = r#"{
        "data":[{"strike":400,"callGEX":11413.858893660656,"putGEX":1724.2438445936816,
          "netGEX":9689.615049066973,"callOI":2075,"putOI":254,"cumulativeGEX":14047.030139945567}],
        "totalGEX":32334.527911347133,
        "callWall":{"strike":400,"gex":11413.858893660656},
        "putWall":{"strike":399,"gex":-3645.2683976446638},
        "gammaInflection":null,"gammaZone":"positive","stockPrice":397.76,
        "meta":{"expirations":["2026-08-10"],"deltaRange":"0.97","formula":"nominal"}
    }"#;

    #[test]
    fn parses_documented_response_fields() {
        let dto: QuantWheelResponseDto = serde_json::from_str(RESPONSE).expect("typed response");
        let snapshot = dto.validate(UnixMs::new(1)).expect("valid snapshot");
        let level = &snapshot.levels[0];
        assert_eq!(snapshot.stock_price, 397.76);
        assert_eq!(level.strike, 400.0);
        assert_eq!(level.call_gex, 11413.858893660656);
        assert_eq!(level.put_gex, 1724.2438445936816);
        assert!((level.net_gex - 9689.615049066973).abs() < 1.0e-9);
        assert_eq!(level.call_open_interest, 2075.0);
        assert_eq!(level.put_open_interest, 254.0);
        assert_eq!(level.cumulative_gex, 14047.030139945567);
        assert_eq!(
            snapshot.call_wall.as_ref().map(|wall| wall.strike),
            Some(400.0)
        );
        assert_eq!(
            snapshot.put_wall.as_ref().map(|wall| wall.strike),
            Some(399.0)
        );
        assert_eq!(snapshot.gamma_inflection, None);
    }

    #[test]
    fn accepts_missing_walls_and_inflection() {
        let body = r#"{"data":[{"strike":400,"callGEX":1,"putGEX":2,"netGEX":-1,
            "callOI":3,"putOI":4,"cumulativeGEX":5}],"totalGEX":-1,
            "callWall":null,"putWall":null,"gammaInflection":null,"stockPrice":400}"#;
        let dto: QuantWheelResponseDto = serde_json::from_str(body).expect("typed response");
        let snapshot = dto.validate(UnixMs::new(1)).expect("valid snapshot");
        assert!(snapshot.call_wall.is_none());
        assert!(snapshot.put_wall.is_none());
        assert!(snapshot.gamma_inflection.is_none());
    }

    #[test]
    fn rejects_empty_data_and_zero_stock_price() {
        let empty: QuantWheelResponseDto =
            serde_json::from_str(r#"{"data":[],"totalGEX":0,"stockPrice":400}"#)
                .expect("typed response");
        assert!(matches!(
            empty.validate(UnixMs::new(1)),
            Err(QuantWheelError::EmptySnapshot)
        ));
        let zero: QuantWheelResponseDto = serde_json::from_str(
            r#"{"data":[{"strike":1,"callGEX":1,"putGEX":1,"netGEX":0,
            "callOI":1,"putOI":1,"cumulativeGEX":0}],"totalGEX":0,"stockPrice":0}"#,
        )
        .expect("typed response");
        assert!(matches!(
            zero.validate(UnixMs::new(1)),
            Err(QuantWheelError::InvalidNumber {
                field: "stockPrice",
                ..
            })
        ));
    }

    #[test]
    fn malformed_json_is_a_decode_error_without_panicking() {
        let result = serde_json::from_str::<QuantWheelResponseDto>("not-json")
            .map_err(QuantWheelError::Decode);
        assert!(matches!(result, Err(QuantWheelError::Decode(_))));
    }

    fn serve_once(
        status: &'static str,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0u8; 4096];
            let count = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /?"));
            assert!(request.contains("ticker=GLD"));
            assert!(request.contains("expirations=2026-08-10"));
            assert!(request.contains("deltaRange=0.97"));
            assert!(request.contains("formula=nominal"));
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).expect("response");
        });
        (format!("http://{address}"), server)
    }

    fn test_config() -> QuantWheelConfig {
        QuantWheelConfig {
            expiration: NaiveDate::from_ymd_opt(2026, 8, 10).expect("date"),
            ..QuantWheelConfig::default()
        }
    }

    #[tokio::test]
    async fn fetch_uses_configured_expiration_and_typed_response() {
        let (url, server) = serve_once("200 OK", RESPONSE);
        let client = QuantWheelGexClient::with_base_url(url, test_config(), None).expect("client");
        let snapshot = client.fetch_gex().await.expect("snapshot");
        assert_eq!(snapshot.levels[0].strike, 400.0);
        server.join().expect("server");
    }

    #[tokio::test]
    async fn http_and_parser_errors_are_returned_without_panicking() {
        let (url, server) = serve_once("503 Service Unavailable", "temporarily unavailable");
        let client = QuantWheelGexClient::with_base_url(url, test_config(), None).expect("client");
        assert!(matches!(
            client.fetch_gex().await,
            Err(QuantWheelError::Http { status: 503, .. })
        ));
        server.join().expect("server");

        let (url, server) = serve_once("200 OK", "not-json");
        let client = QuantWheelGexClient::with_base_url(url, test_config(), None).expect("client");
        assert!(matches!(
            client.fetch_gex().await,
            Err(QuantWheelError::Decode(_))
        ));
        server.join().expect("server");
    }
}
