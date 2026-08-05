use serde::{Deserialize, Serialize};

/// Combined network configuration.
///
/// Both settings take effect after a restart of the application.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Network {
    pub proxy: Option<exchange::proxy::Proxy>,
    pub server_url: Option<String>,
    /// Bearer token for the market-data server.
    /// Stored in the system keychain, never persisted to JSON.
    #[serde(skip)]
    pub server_auth_token: Option<String>,
    pub trade_fetch_mode: TradeFetchMode,
    #[serde(default)]
    pub rithmic: RithmicSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RithmicSettings {
    pub enabled: bool,
    pub url: String,
    pub system_name: String,
    pub app_name: String,
    pub app_version: String,
    #[serde(skip)]
    pub user: Option<String>,
    #[serde(skip)]
    pub password: Option<String>,
}

impl Default for RithmicSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "wss://rituz00100.rithmic.com:443".to_string(),
            system_name: "Rithmic Test".to_string(),
            app_name: "FlowSurface".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            user: None,
            password: None,
        }
    }
}

impl RithmicSettings {
    pub fn adapter_config(&self) -> Option<exchange::adapter::RithmicConfig> {
        self.enabled.then_some(())?;
        Some(exchange::adapter::RithmicConfig {
            url: self.url.clone(),
            system_name: self.system_name.clone(),
            app_name: self.app_name.clone(),
            app_version: self.app_version.clone(),
            user: self.user.clone()?,
            password: self.password.clone()?,
        })
    }
}

impl Network {
    /// Return a copy suitable for disk persistence (proxy auth stripped).
    /// Auth credentials are stored separately in the system keychain.
    pub fn for_persistence(&self) -> Self {
        Self {
            proxy: self.proxy.clone().map(|p| p.without_auth()),
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TradeFetchMode {
    #[default]
    Off,
    /// Direct provider API for venues that expose historical trades.
    Exchange,
    /// Remote Arrow IPC market-data server.
    Server,
}

impl std::fmt::Display for TradeFetchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => write!(f, "Off"),
            Self::Exchange => write!(f, "Exchange"),
            Self::Server => write!(f, "Server"),
        }
    }
}
