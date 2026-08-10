use crate::modal::layout_manager::LayoutManager;
use crate::screen::dashboard::{Dashboard, pane};
use data::{
    UserTimezone,
    layout::{WindowSpec, pane::Axis},
};

use iced::widget::pane_grid::{self, Configuration};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::vec;
use uuid::Uuid;

pub struct Layout {
    pub id: LayoutId,
    pub dashboard: Dashboard,
}

#[derive(Debug, Clone)]
pub struct LayoutId {
    pub unique: Uuid,
    pub name: String,
}

const TEMPLATE_FORMAT: &str = "flowsurface-dashboard-template";
const TEMPLATE_VERSION: u32 = 1;
const MAX_TEMPLATE_BYTES: usize = 5 * 1024 * 1024;
const MAX_TEMPLATE_PANES: usize = 128;
const MAX_TEMPLATE_DEPTH: usize = 32;

#[derive(Serialize, Deserialize)]
struct TemplateFile {
    format: String,
    version: u32,
    layout: data::Layout,
}

pub struct SavedState {
    pub layout_manager: LayoutManager,
    pub main_window: Option<WindowSpec>,
    pub scale_factor: data::ScaleFactor,
    pub timezone: data::UserTimezone,
    pub sidebar: data::Sidebar,
    pub theme: data::Theme,
    pub custom_theme: Option<data::Theme>,
    pub audio_cfg: data::AudioStream,
    pub volume_size_unit: exchange::SizeUnit,
    pub network: data::Network,
    pub debug_terminal_enabled: bool,
    pub cache_market_metadata: bool,
}

pub enum SavedStateLoadOutcome {
    Loaded(SavedState),
    Migrated {
        state: SavedState,
        from_version: u32,
        to_version: u32,
        backup_path: Option<PathBuf>,
    },
    Recovered {
        state: SavedState,
        warnings: Vec<String>,
        backup_path: Option<PathBuf>,
    },
    Corrupt {
        error: String,
        original_path: PathBuf,
        backup_path: Option<PathBuf>,
    },
    MissingDefault(SavedState),
}

impl SavedState {
    pub fn window(&self) -> (iced::window::Position, iced::Size) {
        let position = self.main_window.map(|w| w.position()).map_or(
            iced::window::Position::Centered,
            iced::window::Position::Specific,
        );
        let size = self
            .main_window
            .map_or_else(crate::window::default_size, |w| w.size());

        (position, size)
    }
}

impl Default for SavedState {
    fn default() -> Self {
        SavedState {
            layout_manager: LayoutManager::new(),
            main_window: None,
            scale_factor: data::ScaleFactor::default(),
            timezone: UserTimezone::default(),
            sidebar: data::Sidebar::default(),
            theme: data::Theme::default(),
            custom_theme: None,
            audio_cfg: data::AudioStream::default(),
            volume_size_unit: exchange::SizeUnit::Base,
            network: data::Network::default(),
            debug_terminal_enabled: false,
            cache_market_metadata: false,
        }
    }
}

impl From<&Dashboard> for data::Dashboard {
    fn from(dashboard: &Dashboard) -> Self {
        use pane_grid::Node;

        fn from_layout(panes: &pane_grid::State<pane::State>, node: pane_grid::Node) -> data::Pane {
            match node {
                Node::Split {
                    axis, ratio, a, b, ..
                } => data::Pane::Split {
                    axis: match axis {
                        pane_grid::Axis::Horizontal => Axis::Horizontal,
                        pane_grid::Axis::Vertical => Axis::Vertical,
                    },
                    ratio,
                    a: Box::new(from_layout(panes, *a)),
                    b: Box::new(from_layout(panes, *b)),
                },
                Node::Pane(pane) => panes
                    .get(pane)
                    .map_or(data::Pane::default(), data::Pane::from),
            }
        }

        let main_window_layout = dashboard.panes.layout().clone();

        let popouts_layout: Vec<(data::Pane, WindowSpec)> = dashboard
            .popout
            .iter()
            .map(|(_, (pane, spec))| (from_layout(pane, pane.layout().clone()), *spec))
            .collect();

        data::Dashboard {
            pane: from_layout(&dashboard.panes, main_window_layout),
            popout: {
                popouts_layout
                    .iter()
                    .map(|(pane, window_spec)| (pane.clone(), *window_spec))
                    .collect()
            },
        }
    }
}

pub fn dashboard_from_data(dashboard: data::Dashboard, layout_id: Uuid) -> Dashboard {
    let popout_windows = dashboard
        .popout
        .into_iter()
        .map(|(pane, spec)| (configuration(pane), spec))
        .collect();
    Dashboard::from_config(configuration(dashboard.pane), popout_windows, layout_id)
}

pub fn export_template(layout: &Layout) -> Result<Vec<u8>, String> {
    let file = TemplateFile {
        format: TEMPLATE_FORMAT.to_string(),
        version: TEMPLATE_VERSION,
        layout: data::Layout {
            name: layout.id.name.clone(),
            dashboard: data::Dashboard::from(&layout.dashboard),
        },
    };
    serde_json::to_vec_pretty(&file).map_err(|error| format!("Could not encode template: {error}"))
}

pub fn import_template(bytes: &[u8]) -> Result<data::Layout, String> {
    if bytes.is_empty() {
        return Err("Template file is empty".to_string());
    }
    if bytes.len() > MAX_TEMPLATE_BYTES {
        return Err("Template file is too large".to_string());
    }

    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Template is corrupt or not valid JSON: {error}"))?;
    validate_raw_template(&value)?;
    let file: TemplateFile =
        serde_json::from_value(value).map_err(|error| format!("Template is corrupt: {error}"))?;
    if file.format != TEMPLATE_FORMAT {
        return Err("File is not a FlowSurface dashboard template".to_string());
    }
    if file.version != TEMPLATE_VERSION {
        return Err(format!(
            "Unsupported template version {} (expected {})",
            file.version, TEMPLATE_VERSION
        ));
    }
    validate_template_layout(&file.layout)?;
    Ok(file.layout)
}

fn validate_raw_template(value: &serde_json::Value) -> Result<(), String> {
    let layout = value
        .get("layout")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "Template is missing its layout".to_string())?;
    if !layout.get("name").is_some_and(serde_json::Value::is_string) {
        return Err("Template has an invalid name".to_string());
    }
    let dashboard = layout
        .get("dashboard")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "Template is missing its dashboard".to_string())?;
    let pane = dashboard
        .get("pane")
        .ok_or_else(|| "Template is missing its pane tree".to_string())?;
    serde_json::from_value::<data::Pane>(pane.clone())
        .map_err(|error| format!("Template pane data is corrupt: {error}"))?;

    let popouts = dashboard
        .get("popout")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Template has an invalid popout list".to_string())?;
    for entry in popouts {
        let pair = entry
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| "Template contains a corrupt popout entry".to_string())?;
        serde_json::from_value::<data::Pane>(pair[0].clone())
            .map_err(|error| format!("Template popout pane is corrupt: {error}"))?;
        serde_json::from_value::<WindowSpec>(pair[1].clone())
            .map_err(|error| format!("Template popout window is corrupt: {error}"))?;
    }
    Ok(())
}

fn validate_template_layout(layout: &data::Layout) -> Result<(), String> {
    if layout.name.trim().is_empty() {
        return Err("Template name is empty".to_string());
    }
    let mut panes = 0;
    validate_template_pane(&layout.dashboard.pane, 0, &mut panes)?;
    for (pane, spec) in &layout.dashboard.popout {
        if !spec.width.is_finite()
            || !spec.height.is_finite()
            || !spec.pos_x.is_finite()
            || !spec.pos_y.is_finite()
            || spec.width <= 0.0
            || spec.height <= 0.0
        {
            return Err("Template contains an invalid popout window".to_string());
        }
        validate_template_pane(pane, 0, &mut panes)?;
    }
    Ok(())
}

fn validate_template_pane(
    pane: &data::Pane,
    depth: usize,
    panes: &mut usize,
) -> Result<(), String> {
    if depth > MAX_TEMPLATE_DEPTH {
        return Err("Template pane tree is too deeply nested".to_string());
    }
    *panes += 1;
    if *panes > MAX_TEMPLATE_PANES {
        return Err("Template contains too many panes".to_string());
    }
    if let data::Pane::Split { ratio, a, b, .. } = pane {
        if !ratio.is_finite() || !(0.05..=0.95).contains(ratio) {
            return Err("Template contains an invalid pane split ratio".to_string());
        }
        validate_template_pane(a, depth + 1, panes)?;
        validate_template_pane(b, depth + 1, panes)?;
    }
    Ok(())
}

impl From<&pane::State> for data::Pane {
    fn from(pane: &pane::State) -> Self {
        let streams = pane.streams.clone().into_waiting();

        match &pane.content {
            pane::Content::Starter => data::Pane::Starter {
                link_group: pane.link_group,
            },
            pane::Content::Heatmap {
                chart,
                indicators,
                studies,
                layout,
                ..
            } => data::Pane::HeatmapChart {
                layout: chart.as_ref().map_or(layout.clone(), |c| c.chart_layout()),
                stream_type: streams,
                settings: pane.settings.clone(),
                indicators: indicators.clone(),
                studies: chart
                    .as_ref()
                    .map_or(studies.clone(), |c| c.studies.clone()),
                link_group: pane.link_group,
            },
            pane::Content::ShaderHeatmap {
                indicators,
                studies,
                ..
            } => data::Pane::ShaderHeatmap {
                stream_type: streams,
                studies: studies.clone(),
                indicators: indicators.clone(),
                settings: pane.settings.clone(),
                link_group: pane.link_group,
            },
            pane::Content::Kline {
                chart,
                indicators,
                kind,
                layout,
                drawings,
                ..
            } => data::Pane::KlineChart {
                layout: chart.as_ref().map_or(layout.clone(), |c| c.chart_layout()),
                kind: kind.clone(),
                drawings: chart.as_ref().map_or(drawings.clone(), |c| c.drawings()),
                stream_type: streams,
                settings: pane.settings.clone(),
                indicators: indicators.clone(),
                link_group: pane.link_group,
            },
            pane::Content::Gex {
                chart,
                underlying,
                liquidity_reference,
                ..
            } => {
                let settings = data::layout::pane::Settings {
                    visual_config: chart
                        .as_ref()
                        .map(|chart| data::layout::pane::VisualConfig::Gex(*chart.config())),
                    ..pane.settings.clone()
                };
                data::Pane::GexChart {
                    underlying: *underlying,
                    liquidity_reference: chart
                        .as_ref()
                        .and_then(crate::chart::gex::GexChart::liquidity_reference)
                        .or(*liquidity_reference),
                    settings,
                    link_group: pane.link_group,
                }
            }
            pane::Content::TimeAndSales(_) => data::Pane::TimeAndSales {
                stream_type: streams,
                settings: pane.settings.clone(),
                link_group: pane.link_group,
            },
            pane::Content::Ladder(_) => data::Pane::Ladder {
                stream_type: streams,
                settings: pane.settings.clone(),
                link_group: pane.link_group,
            },
            pane::Content::Comparison(chart) => {
                let settings = data::layout::pane::Settings {
                    visual_config: chart.as_ref().map(|c| {
                        data::layout::pane::VisualConfig::Comparison(c.serializable_config())
                    }),
                    ..pane.settings.clone()
                };

                data::Pane::ComparisonChart {
                    stream_type: streams,
                    settings,
                    link_group: pane.link_group,
                }
            }
        }
    }
}

pub fn configuration(pane: data::Pane) -> Configuration<pane::State> {
    match pane {
        data::Pane::Split { axis, ratio, a, b } => Configuration::Split {
            axis: match axis {
                Axis::Horizontal => pane_grid::Axis::Horizontal,
                Axis::Vertical => pane_grid::Axis::Vertical,
            },
            ratio,
            a: Box::new(configuration(*a)),
            b: Box::new(configuration(*b)),
        },
        data::Pane::Starter { link_group } => Configuration::Pane(pane::State::from_config(
            pane::Content::Starter,
            vec![],
            data::layout::pane::Settings::default(),
            link_group,
        )),
        data::Pane::ShaderHeatmap {
            stream_type,
            settings,
            indicators,
            studies,
            link_group,
        } => {
            let content = pane::Content::ShaderHeatmap {
                chart: None,
                indicators: indicators.clone(),
                studies,
            };

            Configuration::Pane(pane::State::from_config(
                content,
                stream_type,
                settings,
                link_group,
            ))
        }
        data::Pane::HeatmapChart {
            layout,
            studies,
            stream_type,
            settings,
            indicators,
            link_group,
        } => {
            let content = pane::Content::Heatmap {
                chart: None,
                indicators: indicators.clone(),
                layout,
                studies,
            };

            Configuration::Pane(pane::State::from_config(
                content,
                stream_type,
                settings,
                link_group,
            ))
        }
        data::Pane::KlineChart {
            layout,
            kind,
            drawings,
            stream_type,
            settings,
            indicators,
            link_group,
        } => {
            let content = pane::Content::Kline {
                chart: None,
                indicators: indicators.clone(),
                layout,
                kind,
                drawings,
            };

            Configuration::Pane(pane::State::from_config(
                content,
                stream_type,
                settings,
                link_group,
            ))
        }
        data::Pane::GexChart {
            underlying,
            liquidity_reference,
            settings,
            link_group,
        } => {
            let config = settings
                .visual_config
                .as_ref()
                .and_then(data::layout::pane::VisualConfig::gex);
            let content = pane::Content::Gex {
                chart: Some(crate::chart::gex::GexChart::new(
                    underlying,
                    config,
                    liquidity_reference,
                )),
                underlying,
                liquidity_reference,
                liquidity_reference_source: liquidity_reference
                    .map(|_| pane::GexLiquidityReferenceSource::Persisted),
                unsupported: false,
            };
            Configuration::Pane(pane::State::from_config(
                content,
                vec![],
                settings,
                link_group,
            ))
        }
        data::Pane::ComparisonChart {
            stream_type,
            settings,
            link_group,
        } => {
            let content = pane::Content::Comparison(None);

            Configuration::Pane(pane::State::from_config(
                content,
                stream_type,
                settings,
                link_group,
            ))
        }
        data::Pane::TimeAndSales {
            stream_type,
            settings,
            link_group,
        } => {
            let content = pane::Content::TimeAndSales(None);

            Configuration::Pane(pane::State::from_config(
                content,
                stream_type,
                settings,
                link_group,
            ))
        }
        data::Pane::Ladder {
            stream_type,
            settings,
            link_group,
        } => {
            let content = pane::Content::Ladder(None);

            Configuration::Pane(pane::State::from_config(
                content,
                stream_type,
                settings,
                link_group,
            ))
        }
    }
}

pub fn load_saved_state() -> SavedStateLoadOutcome {
    match data::load_saved_state_file() {
        data::StateLoadOutcome::Loaded(state) => {
            SavedStateLoadOutcome::Loaded(saved_state_from_config(state))
        }
        data::StateLoadOutcome::Migrated {
            state,
            from_version,
            to_version,
            backup_path,
        } => SavedStateLoadOutcome::Migrated {
            state: saved_state_from_config(state),
            from_version,
            to_version,
            backup_path,
        },
        data::StateLoadOutcome::Recovered {
            state,
            warnings,
            backup_path,
        } => SavedStateLoadOutcome::Recovered {
            state: saved_state_from_config(state),
            warnings,
            backup_path,
        },
        data::StateLoadOutcome::Corrupt {
            error,
            original_path,
            backup_path,
        } => {
            log::error!("SAVED_STATE Corrupt | action=await_user_confirmation error={error}");
            SavedStateLoadOutcome::Corrupt {
                error,
                original_path,
                backup_path,
            }
        }
        data::StateLoadOutcome::MissingDefault(state) => {
            SavedStateLoadOutcome::MissingDefault(saved_state_from_config(state))
        }
    }
}

fn saved_state_from_config(state: data::State) -> SavedState {
    let mut de_layouts = vec![];

    for layout in &state.layout_manager.layouts {
        let mut popout_windows = Vec::new();

        for (pane, window_spec) in &layout.dashboard.popout {
            let configuration = configuration(pane.clone());
            popout_windows.push((configuration, *window_spec));
        }

        let layout_id = Uuid::new_v4();

        let dashboard = Dashboard::from_config(
            configuration(layout.dashboard.pane.clone()),
            popout_windows,
            layout_id,
        );

        de_layouts.push((layout.name.clone(), layout_id, dashboard));
    }

    let layout_manager = {
        let mut layouts = Vec::with_capacity(de_layouts.len());

        for (name, layout_id, dashboard) in de_layouts {
            let id = LayoutId {
                unique: layout_id,
                name,
            };
            layouts.push(Layout { id, dashboard });
        }

        if layouts.is_empty() {
            log::error!("Saved state contained no layouts. Starting with a default layout.");
            LayoutManager::new()
        } else {
            let active_layout =
                state
                    .layout_manager
                    .active_layout
                    .as_ref()
                    .and_then(|target_name| {
                        layouts
                            .iter()
                            .find(|layout| layout.id.name == *target_name)
                            .map(|layout| layout.id.clone())
                    });

            LayoutManager::from_config(layouts, active_layout)
        }
    };

    let mut network = state.network.clone();
    if network.server_auth_token.is_none()
        && let Some(ref url) = network.server_url
    {
        network.server_auth_token = data::config::auth::load_server_token(url);
    }

    crate::connector::fetcher::set_trade_fetch_mode(network.trade_fetch_mode.clone());
    exchange::unit::qty::set_preferred_currency(state.size_in_quote_ccy);

    if let Some(proxy) = network.proxy.as_mut()
        && proxy.auth().is_none()
        && let Some(auth) = data::config::auth::load_proxy_auth(proxy)
    {
        proxy.set_auth(Some(auth));
    }

    SavedState {
        theme: state.selected_theme,
        custom_theme: state.custom_theme,
        layout_manager,
        main_window: state.main_window,
        timezone: state.timezone,
        sidebar: state.sidebar,
        scale_factor: state.scale_factor,
        audio_cfg: state.audio_cfg,
        volume_size_unit: state.size_in_quote_ccy,
        network,
        debug_terminal_enabled: state.debug_terminal_enabled,
        cache_market_metadata: state.cache_market_metadata,
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;

    #[test]
    fn template_round_trip_preserves_name_and_dashboard() {
        let id = Uuid::new_v4();
        let layout = Layout {
            id: LayoutId {
                unique: id,
                name: "Scalping".to_string(),
            },
            dashboard: Dashboard::empty(id),
        };

        let encoded = export_template(&layout).expect("export template");
        let imported = import_template(&encoded).expect("import template");

        assert_eq!(imported.name, "Scalping");
        assert!(matches!(
            imported.dashboard.pane,
            data::Pane::Starter { .. }
        ));
    }

    #[test]
    fn corrupt_template_pane_is_rejected_instead_of_defaulted() {
        let id = Uuid::new_v4();
        let layout = Layout {
            id: LayoutId {
                unique: id,
                name: "Broken".to_string(),
            },
            dashboard: Dashboard::empty(id),
        };
        let encoded = export_template(&layout).expect("export template");
        let mut value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("decode exported template");
        value["layout"]["dashboard"]["pane"] = serde_json::json!({ "UnknownPane": {} });

        let corrupt = serde_json::to_vec(&value).expect("encode corrupt fixture");
        assert!(import_template(&corrupt).is_err());
    }

    #[test]
    fn unrelated_json_is_not_accepted_as_a_template() {
        assert!(import_template(br#"{"name":"not a template"}"#).is_err());
    }
}
