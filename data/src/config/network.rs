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
    /// QuantWheel Auth.js session cookie. Stored in the system keychain and
    /// never persisted to the saved-state JSON.
    #[serde(skip)]
    pub quantwheel_session_cookie: Option<String>,
    pub trade_fetch_mode: TradeFetchMode,
}

impl Network {
    /// Compare the settings used by clients that are created at startup.
    pub fn requires_restart_from(&self, running: &Self) -> bool {
        self.proxy != running.proxy
            || self.server_url != running.server_url
            || self.server_auth_token != running.server_auth_token
            || self.trade_fetch_mode != running.trade_fetch_mode
    }

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
    /// Direct exchange API only (Binance spot/linear/inverse).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverting_saved_network_settings_clears_restart_even_after_live_login() {
        let running = Network::default();
        let mut saved = Network {
            trade_fetch_mode: TradeFetchMode::Exchange,
            ..running.clone()
        };
        assert!(saved.requires_restart_from(&running));
        saved.quantwheel_session_cookie = Some("synthetic-session".into());
        saved.trade_fetch_mode = running.trade_fetch_mode.clone();
        assert!(!saved.requires_restart_from(&running));
    }
}
