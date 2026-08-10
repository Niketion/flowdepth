use super::ScaleFactor;
use super::sidebar::Sidebar;
use super::timezone::UserTimezone;
use crate::config::network::Network;
use crate::layout::WindowSpec;
use crate::{AudioStream, Layout, Theme};

use serde::{Deserialize, Serialize};

pub const CURRENT_SAVED_STATE_VERSION: u32 = 1;

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Layouts {
    pub layouts: Vec<Layout>,
    pub active_layout: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct State {
    pub saved_state_version: u32,
    pub layout_manager: Layouts,
    pub selected_theme: Theme,
    pub custom_theme: Option<Theme>,
    pub main_window: Option<WindowSpec>,
    pub timezone: UserTimezone,
    pub sidebar: Sidebar,
    pub scale_factor: ScaleFactor,
    pub audio_cfg: AudioStream,
    pub network: Network,
    pub size_in_quote_ccy: exchange::SizeUnit,
    pub debug_terminal_enabled: bool,
    pub cache_market_metadata: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            saved_state_version: CURRENT_SAVED_STATE_VERSION,
            layout_manager: Layouts::default(),
            selected_theme: Theme::default(),
            custom_theme: None,
            main_window: None,
            timezone: UserTimezone::default(),
            sidebar: Sidebar::default(),
            scale_factor: ScaleFactor::default(),
            audio_cfg: AudioStream::default(),
            network: Network::default(),
            size_in_quote_ccy: exchange::SizeUnit::Base,
            debug_terminal_enabled: false,
            cache_market_metadata: false,
        }
    }
}

impl State {
    pub fn from_parts(
        layout_manager: Layouts,
        selected_theme: Theme,
        custom_theme: Option<Theme>,
        main_window: Option<WindowSpec>,
        timezone: UserTimezone,
        sidebar: Sidebar,
        scale_factor: ScaleFactor,
        audio_cfg: AudioStream,
        network: Network,
        volume_size_unit: exchange::SizeUnit,
        debug_terminal_enabled: bool,
        cache_market_metadata: bool,
    ) -> Self {
        State {
            saved_state_version: CURRENT_SAVED_STATE_VERSION,
            layout_manager,
            selected_theme: Theme(selected_theme.0),
            custom_theme: custom_theme.map(|t| Theme(t.0)),
            main_window,
            timezone,
            sidebar,
            scale_factor,
            audio_cfg,
            network,
            size_in_quote_ccy: volume_size_unit,
            debug_terminal_enabled,
            cache_market_metadata,
        }
    }
}
