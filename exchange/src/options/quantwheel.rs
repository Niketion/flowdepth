use super::{OptionsProvider, OptionsUnderlying};
use crate::{UnixMs, adapter};
use chrono::{Days, NaiveDate, Utc};
use reqwest::{
    Client,
    cookie::{CookieStore, Jar},
    header::HeaderMap,
};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use thiserror::Error;

const PRODUCTION_BASE_URL: &str = "https://quantwheel.com/api/tools/gex";
const PRODUCTION_EXPIRATIONS_URL: &str = "https://quantwheel.com/api/options/expirations";
const PRODUCTION_ORIGIN: &str = "https://quantwheel.com";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// QuantWheel's public pricing and GEX pages advertise five anonymous results
/// per feature, with the remaining calculation count resetting daily.
pub const ANONYMOUS_DAILY_GEX_LIMIT: u16 = 5;
const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuantWheelQuota {
    pub limit: u16,
    pub limit_is_estimate: bool,
    pub remaining: Option<u16>,
    pub reset_at: UnixMs,
    pub reset_is_estimate: bool,
    pub authenticated: bool,
}

impl QuantWheelQuota {
    pub fn used(self) -> Option<u16> {
        self.remaining
            .map(|remaining| self.limit.saturating_sub(remaining.min(self.limit)))
    }

    fn from_headers(headers: &HeaderMap, now: UnixMs, authenticated: bool) -> Self {
        let reported_limit =
            header_u64(headers, "x-ratelimit-limit").and_then(|value| u16::try_from(value).ok());
        let limit = reported_limit.unwrap_or(ANONYMOUS_DAILY_GEX_LIMIT);
        let remaining = header_u64(headers, "x-ratelimit-remaining")
            .and_then(|value| u16::try_from(value).ok());
        let explicit_reset = header_u64(headers, "x-ratelimit-reset").map(|value| {
            // Rate-limit reset headers conventionally use epoch seconds. Accept
            // epoch milliseconds too so the client remains robust if that changes.
            UnixMs::new(if value < 10_000_000_000 {
                value.saturating_mul(1_000)
            } else {
                value
            })
        });
        let retry_after = header_u64(headers, "retry-after")
            .map(|seconds| now.saturating_add(seconds.saturating_mul(1_000)));
        let reset_at = explicit_reset
            .or(retry_after)
            .unwrap_or_else(|| next_utc_midnight(now));
        Self {
            limit,
            limit_is_estimate: authenticated && reported_limit.is_none(),
            remaining,
            reset_at,
            reset_is_estimate: explicit_reset.is_none() && retry_after.is_none(),
            authenticated,
        }
    }
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

fn next_utc_midnight(now: UnixMs) -> UnixMs {
    UnixMs::new(
        now.as_u64()
            .checked_div(DAY_MS)
            .unwrap_or(0)
            .saturating_add(1)
            .saturating_mul(DAY_MS),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantWheelConfig {
    /// Optional deterministic override used by controlled integrations. The
    /// production client discovers valid expirations from QuantWheel.
    pub expiration_override: Option<NaiveDate>,
    pub delta_range: String,
    pub formula: String,
}

impl Default for QuantWheelConfig {
    fn default() -> Self {
        Self {
            expiration_override: None,
            delta_range: "0.97".to_owned(),
            formula: "nominal".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuantWheelExpirySelection {
    NextExpiry,
    ThroughDays(u16),
    All,
}

#[derive(Debug, Error)]
pub enum QuantWheelError {
    #[error("failed to build QuantWheel HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("QuantWheel HTTP request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("QuantWheel returned HTTP {status}: {message}")]
    Http {
        status: u16,
        message: String,
        quota: QuantWheelQuota,
    },
    #[error("invalid QuantWheel JSON response: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("QuantWheel returned no GEX levels")]
    EmptySnapshot,
    #[error("QuantWheel returned invalid {field}: {value}")]
    InvalidNumber { field: &'static str, value: f64 },
    #[error("QuantWheel returned no usable expirations for the requested underlying")]
    NoExpirations,
    #[error("QuantWheel authentication failed: {0}")]
    Auth(String),
}

impl QuantWheelError {
    pub fn quota(&self) -> Option<QuantWheelQuota> {
        match self {
            Self::Http { quota, .. } => Some(*quota),
            _ => None,
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::Http { status: 429, .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuantWheelGexResponse {
    pub snapshot: QuantWheelGexSnapshot,
    pub quota: QuantWheelQuota,
    pub expirations: Arc<[NaiveDate]>,
}

#[derive(Debug, Clone)]
pub struct QuantWheelGexClient {
    client: Client,
    cookie_jar: Arc<Jar>,
    base_url: String,
    expirations_url: String,
    origin_url: reqwest::Url,
    config: QuantWheelConfig,
}

impl QuantWheelGexClient {
    pub fn new(
        proxy: Option<&adapter::Proxy>,
        session_cookie: Option<&str>,
    ) -> Result<Self, QuantWheelError> {
        Self::with_urls(
            PRODUCTION_BASE_URL,
            PRODUCTION_EXPIRATIONS_URL,
            PRODUCTION_ORIGIN,
            QuantWheelConfig::default(),
            proxy,
            session_cookie,
        )
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        config: QuantWheelConfig,
        proxy: Option<&adapter::Proxy>,
    ) -> Result<Self, QuantWheelError> {
        let base_url = base_url.into();
        Self::with_urls(
            base_url.clone(),
            base_url.clone(),
            &base_url,
            config,
            proxy,
            None,
        )
    }

    fn with_urls(
        base_url: impl Into<String>,
        expirations_url: impl Into<String>,
        origin_url: &str,
        config: QuantWheelConfig,
        proxy: Option<&adapter::Proxy>,
        session_cookie: Option<&str>,
    ) -> Result<Self, QuantWheelError> {
        let origin_url = reqwest::Url::parse(origin_url)
            .map_err(|error| QuantWheelError::Auth(error.to_string()))?;
        let cookie_jar = Arc::new(Jar::default());
        if let Some(cookie) = session_cookie.filter(|cookie| !cookie.trim().is_empty()) {
            cookie_jar.add_cookie_str(cookie, &origin_url);
        }
        let builder = Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .cookie_provider(cookie_jar.clone());
        let client = adapter::proxy::try_apply_proxy(builder, proxy)
            .build()
            .map_err(QuantWheelError::Client)?;
        Ok(Self {
            client,
            cookie_jar,
            base_url: base_url.into(),
            expirations_url: expirations_url.into(),
            origin_url,
            config,
        })
    }

    pub async fn fetch_gex(
        &self,
        underlying: OptionsUnderlying,
        selection: QuantWheelExpirySelection,
    ) -> Result<QuantWheelGexResponse, QuantWheelError> {
        if !matches!(underlying, OptionsUnderlying::Gld | OptionsUnderlying::Ndx) {
            return Err(QuantWheelError::Auth(format!(
                "unsupported QuantWheel underlying {underlying}"
            )));
        }
        let expirations = if let Some(expiration) = self.config.expiration_override {
            vec![expiration]
        } else {
            let available = self.fetch_expirations(underlying).await?;
            select_expirations(&available, Utc::now().date_naive(), selection)?
        };
        let expiration_query = expirations
            .iter()
            .map(NaiveDate::to_string)
            .collect::<Vec<_>>()
            .join(",");
        log::info!(
            "GEX FetchStarted kind=snapshot underlying={underlying} provider=QuantWheel auth={} selection={selection:?} expirations={expiration_query}",
            if self.session_cookie().is_some() {
                "session"
            } else {
                "anonymous"
            }
        );
        let response = self
            .client
            .get(&self.base_url)
            .query(&[
                ("ticker", underlying.as_str()),
                ("expirations", expiration_query.as_str()),
                ("deltaRange", self.config.delta_range.as_str()),
                ("formula", self.config.formula.as_str()),
            ])
            .send()
            .await
            .map_err(QuantWheelError::Request)?;
        let status = response.status();
        let authenticated = self.session_cookie().is_some();
        let mut quota =
            QuantWheelQuota::from_headers(response.headers(), UnixMs::now(), authenticated);
        if status.as_u16() == 429 && quota.remaining.is_none() {
            quota.remaining = Some(0);
        }
        let body = response.text().await.map_err(QuantWheelError::Request)?;
        // Knowledge time for the live overlay is the earliest reliable local
        // point at which the complete successful response body is available.
        // Capture it before JSON decoding and validation so parse cost cannot
        // backdate or delay the observation semantically.
        let observed_at = UnixMs::now();
        if !status.is_success() {
            return Err(QuantWheelError::Http {
                status: status.as_u16(),
                message: body.chars().take(256).collect(),
                quota,
            });
        }
        let dto: QuantWheelResponseDto =
            serde_json::from_str(&body).map_err(QuantWheelError::Decode)?;
        let snapshot = dto.validate(underlying, observed_at)?;
        log::info!(
            "GEX SnapshotRefreshed underlying={underlying} provider=QuantWheel levels={} expirations={} observed_at={}",
            snapshot.levels.len(),
            expiration_query,
            snapshot.observed_at
        );
        Ok(QuantWheelGexResponse {
            snapshot,
            quota,
            expirations: expirations.into(),
        })
    }

    async fn fetch_expirations(
        &self,
        underlying: OptionsUnderlying,
    ) -> Result<Vec<NaiveDate>, QuantWheelError> {
        let response = self
            .client
            .get(&self.expirations_url)
            .query(&[("ticker", underlying.as_str())])
            .send()
            .await
            .map_err(QuantWheelError::Request)?;
        let status = response.status();
        let body = response.text().await.map_err(QuantWheelError::Request)?;
        if !status.is_success() {
            return Err(QuantWheelError::Auth(format!(
                "expiration discovery returned HTTP {}: {}",
                status.as_u16(),
                body.chars().take(256).collect::<String>()
            )));
        }
        let dto: QuantWheelExpirationsDto =
            serde_json::from_str(&body).map_err(QuantWheelError::Decode)?;
        let mut expirations = dto
            .expirations
            .into_iter()
            .filter_map(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok())
            .collect::<Vec<_>>();
        expirations.sort_unstable();
        expirations.dedup();
        Ok(expirations)
    }

    pub async fn request_login_code(&self, email: &str) -> Result<(), QuantWheelError> {
        let email = email.trim().to_ascii_lowercase();
        if email.is_empty() {
            return Err(QuantWheelError::Auth("email is required".to_owned()));
        }
        let validation = self
            .client
            .post(self.origin_url.join("/api/auth/validate-email").unwrap())
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .map_err(QuantWheelError::Request)?;
        if !validation.status().is_success() {
            return Err(QuantWheelError::Auth(response_message(validation).await));
        }
        let csrf: QuantWheelCsrfDto = self
            .client
            .get(self.origin_url.join("/api/auth/csrf").unwrap())
            .send()
            .await
            .map_err(QuantWheelError::Request)?
            .json()
            .await
            .map_err(QuantWheelError::Request)?;
        let response = self
            .client
            .post(self.origin_url.join("/api/auth/signin/resend").unwrap())
            .header("X-Auth-Return-Redirect", "1")
            .form(&[
                ("email", email.as_str()),
                ("csrfToken", csrf.csrf_token.as_str()),
                ("callbackUrl", PRODUCTION_ORIGIN),
            ])
            .send()
            .await
            .map_err(QuantWheelError::Request)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(QuantWheelError::Auth(response_message(response).await))
        }
    }

    pub async fn verify_login_code(
        &self,
        email: &str,
        code: &str,
    ) -> Result<String, QuantWheelError> {
        let response = self
            .client
            .post(self.origin_url.join("/api/auth/email-code/verify").unwrap())
            .json(&serde_json::json!({
                "email": email.trim().to_ascii_lowercase(),
                "code": code.trim(),
                "callbackUrl": PRODUCTION_ORIGIN,
            }))
            .send()
            .await
            .map_err(QuantWheelError::Request)?;
        if !response.status().is_success() {
            return Err(QuantWheelError::Auth(response_message(response).await));
        }
        let callback: QuantWheelLoginCallbackDto =
            response.json().await.map_err(QuantWheelError::Request)?;
        self.client
            .get(callback.url)
            .send()
            .await
            .map_err(QuantWheelError::Request)?
            .error_for_status()
            .map_err(QuantWheelError::Request)?;
        let session: QuantWheelSessionDto = self
            .client
            .get(self.origin_url.join("/api/auth/session").unwrap())
            .send()
            .await
            .map_err(QuantWheelError::Request)?
            .json()
            .await
            .map_err(QuantWheelError::Request)?;
        if session.user.is_none() {
            return Err(QuantWheelError::Auth(
                "the verification completed without an authenticated session".to_owned(),
            ));
        }
        self.session_cookie().ok_or_else(|| {
            QuantWheelError::Auth("QuantWheel did not return a session cookie".to_owned())
        })
    }

    fn session_cookie(&self) -> Option<String> {
        self.cookie_jar
            .cookies(&self.origin_url)?
            .to_str()
            .ok()?
            .split(';')
            .map(str::trim)
            .find(|cookie| cookie.starts_with("__Secure-authjs.session-token="))
            .map(str::to_owned)
    }
}

async fn response_message(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            format!(
                "HTTP {}: {}",
                status.as_u16(),
                body.chars().take(256).collect::<String>()
            )
        })
}

fn select_expirations(
    available: &[NaiveDate],
    today: NaiveDate,
    selection: QuantWheelExpirySelection,
) -> Result<Vec<NaiveDate>, QuantWheelError> {
    let future = available
        .iter()
        .copied()
        .filter(|expiration| *expiration >= today)
        .collect::<Vec<_>>();
    let selected = match selection {
        QuantWheelExpirySelection::NextExpiry => future.first().copied().into_iter().collect(),
        QuantWheelExpirySelection::All => future,
        QuantWheelExpirySelection::ThroughDays(days) => {
            let target = today
                .checked_add_days(Days::new(u64::from(days)))
                .ok_or(QuantWheelError::NoExpirations)?;
            let boundary = future
                .iter()
                .copied()
                .find(|expiration| *expiration >= target)
                .ok_or(QuantWheelError::NoExpirations)?;
            future
                .into_iter()
                .take_while(|expiration| *expiration <= boundary)
                .collect()
        }
    };
    (!selected.is_empty())
        .then_some(selected)
        .ok_or(QuantWheelError::NoExpirations)
}

#[derive(Debug, Deserialize)]
struct QuantWheelExpirationsDto {
    expirations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuantWheelCsrfDto {
    csrf_token: String,
}

#[derive(Debug, Deserialize)]
struct QuantWheelLoginCallbackDto {
    url: String,
}

#[derive(Debug, Deserialize)]
struct QuantWheelSessionDto {
    user: Option<serde_json::Value>,
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
    fn validate(
        self,
        underlying: OptionsUnderlying,
        observed_at: UnixMs,
    ) -> Result<QuantWheelGexSnapshot, QuantWheelError> {
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
            underlying,
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
        let snapshot = dto
            .validate(OptionsUnderlying::Gld, UnixMs::new(1))
            .expect("valid snapshot");
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
        let snapshot = dto
            .validate(OptionsUnderlying::Gld, UnixMs::new(1))
            .expect("valid snapshot");
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
            empty.validate(OptionsUnderlying::Gld, UnixMs::new(1)),
            Err(QuantWheelError::EmptySnapshot)
        ));
        let zero: QuantWheelResponseDto = serde_json::from_str(
            r#"{"data":[{"strike":1,"callGEX":1,"putGEX":1,"netGEX":0,
            "callOI":1,"putOI":1,"cumulativeGEX":0}],"totalGEX":0,"stockPrice":0}"#,
        )
        .expect("typed response");
        assert!(matches!(
            zero.validate(OptionsUnderlying::Gld, UnixMs::new(1)),
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
        ticker: &'static str,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0u8; 4096];
            let count = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /?"));
            assert!(request.contains(&format!("ticker={ticker}")));
            assert!(request.contains("expirations=2026-08-10"));
            assert!(request.contains("deltaRange=0.97"));
            assert!(request.contains("formula=nominal"));
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-RateLimit-Remaining: 3\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).expect("response");
        });
        (format!("http://{address}"), server)
    }

    fn test_config() -> QuantWheelConfig {
        QuantWheelConfig {
            expiration_override: Some(NaiveDate::from_ymd_opt(2026, 8, 10).expect("date")),
            ..QuantWheelConfig::default()
        }
    }

    #[tokio::test]
    async fn fetch_uses_configured_expiration_and_typed_response() {
        let (url, server) = serve_once("200 OK", RESPONSE, "GLD");
        let client = QuantWheelGexClient::with_base_url(url, test_config(), None).expect("client");
        let response = client
            .fetch_gex(
                OptionsUnderlying::Gld,
                QuantWheelExpirySelection::NextExpiry,
            )
            .await
            .expect("snapshot");
        assert_eq!(response.snapshot.levels[0].strike, 400.0);
        assert_eq!(response.quota.limit, 5);
        assert_eq!(response.quota.remaining, Some(3));
        assert_eq!(response.quota.used(), Some(2));
        assert!(response.quota.reset_is_estimate);
        server.join().expect("server");
    }

    #[tokio::test]
    async fn fetch_ndx_uses_ndx_ticker_and_preserves_underlying() {
        let (url, server) = serve_once("200 OK", RESPONSE, "NDX");
        let client = QuantWheelGexClient::with_base_url(url, test_config(), None).expect("client");
        let response = client
            .fetch_gex(
                OptionsUnderlying::Ndx,
                QuantWheelExpirySelection::NextExpiry,
            )
            .await
            .expect("snapshot");
        assert_eq!(response.snapshot.underlying, OptionsUnderlying::Ndx);
        server.join().expect("server");
    }

    #[tokio::test]
    async fn http_and_parser_errors_are_returned_without_panicking() {
        let (url, server) = serve_once("503 Service Unavailable", "temporarily unavailable", "GLD");
        let client = QuantWheelGexClient::with_base_url(url, test_config(), None).expect("client");
        assert!(matches!(
            client
                .fetch_gex(
                    OptionsUnderlying::Gld,
                    QuantWheelExpirySelection::NextExpiry
                )
                .await,
            Err(QuantWheelError::Http { status: 503, .. })
        ));
        server.join().expect("server");

        let (url, server) = serve_once("200 OK", "not-json", "GLD");
        let client = QuantWheelGexClient::with_base_url(url, test_config(), None).expect("client");
        assert!(matches!(
            client
                .fetch_gex(
                    OptionsUnderlying::Gld,
                    QuantWheelExpirySelection::NextExpiry
                )
                .await,
            Err(QuantWheelError::Decode(_))
        ));
        server.join().expect("server");
    }

    #[test]
    fn estimated_daily_reset_is_next_utc_midnight() {
        let now = UnixMs::new(1_800_000_012_345);
        let quota = QuantWheelQuota::from_headers(&HeaderMap::new(), now, false);
        assert_eq!(quota.reset_at.as_u64() % DAY_MS, 0);
        assert!(quota.reset_at > now);
        assert!(quota.reset_at.saturating_diff(now) <= DAY_MS);
        assert!(quota.reset_is_estimate);
    }
}
