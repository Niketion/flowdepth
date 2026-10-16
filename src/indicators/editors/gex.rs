use super::config_message;
use crate::screen::dashboard::pane;
use crate::screen::dashboard::pane::Message;
use crate::widget::{labeled_slider, pick_list};
use data::chart::kline::Config as KlineConfig;
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, row, text},
};

pub(super) fn view<'a>(
    pane: pane_grid::Pane,
    cfg: KlineConfig,
    state: &'a pane::State,
    advanced: bool,
) -> Element<'a, Message> {
    let mut sections = column![];

    use data::chart::gex::{GexExpiryFilter, GexGammaSource, GexLevelColor, GexLevelsConfig};
    let levels = cfg.gex_levels();
    let proxy_available = matches!(
        &state.content,
        pane::Content::Kline { chart: Some(chart), .. } if chart.gex_proxy_available()
    );
    let provider_status = if proxy_available {
        "24h proxy history: GEX Monitor, Live profile: Deribit"
    } else {
        "Live profile: Deribit, Local history only"
    };
    let update = move |next: GexLevelsConfig| config_message(pane, cfg.with_gex_levels(next));
    let toggle = |label: &'static str, current: bool, change: fn(&mut GexLevelsConfig, bool)| {
        checkbox(current).label(label).on_toggle(move |value| {
            let mut next = levels;
            change(&mut next, value);
            update(next)
        })
    };
    let expiry = pick_list(
        GexExpiryFilter::ALL,
        Some(levels.expiry_filter),
        move |expiry_filter| {
            update(GexLevelsConfig {
                expiry_filter,
                ..levels
            })
        },
    );
    let gamma_source = pick_list(
        GexGammaSource::ALL,
        Some(levels.gamma_source),
        move |gamma_source| {
            update(GexLevelsConfig {
                gamma_source,
                ..levels
            })
        },
    );
    let positive_color = pick_list(
        GexLevelColor::ALL,
        Some(levels.positive_color),
        move |positive_color| {
            update(GexLevelsConfig {
                positive_color,
                ..levels
            })
        },
    );
    let negative_color = pick_list(
        GexLevelColor::ALL,
        Some(levels.negative_color),
        move |negative_color| {
            update(GexLevelsConfig {
                negative_color,
                ..levels
            })
        },
    );
    let minimum_oi = labeled_slider(
        "Minimum OI",
        0.0..=1_000.0,
        levels.minimum_open_interest as f32,
        move |value| {
            update(GexLevelsConfig {
                minimum_open_interest: f64::from(value),
                ..levels
            })
        },
        |value| format!("{value:.0}"),
        Some(5.0),
    );
    let minimum_gex = labeled_slider(
        "Minimum absolute GEX",
        0.0..=1_000_000.0,
        levels.minimum_absolute_gex as f32,
        move |value| {
            update(GexLevelsConfig {
                minimum_absolute_gex: f64::from(value),
                ..levels
            })
        },
        |value| format!("{value:.0}"),
        Some(1_000.0),
    );
    let minimum_strength = labeled_slider(
        "Minimum zone strength",
        0.05..=0.40,
        levels.minimum_zone_strength,
        move |minimum_zone_strength| {
            update(GexLevelsConfig {
                minimum_zone_strength,
                ..levels
            })
        },
        |value| format!("{value:.2}"),
        Some(0.01),
    );
    let level_match_tolerance = labeled_slider(
        "Level match tolerance",
        0.0..=1.0,
        levels.level_match_tolerance_percent,
        move |level_match_tolerance_percent| {
            update(GexLevelsConfig {
                level_match_tolerance_percent,
                ..levels
            })
        },
        |value| format!("{value:.2}%"),
        Some(0.01),
    );
    let max_positive = labeled_slider(
        "Maximum positive zones",
        1.0..=6.0,
        f32::from(levels.max_positive_zones),
        move |value| {
            update(GexLevelsConfig {
                max_positive_zones: value as u8,
                ..levels
            })
        },
        |value| format!("{value:.0}"),
        Some(1.0),
    );
    let max_negative = labeled_slider(
        "Maximum negative zones",
        1.0..=6.0,
        f32::from(levels.max_negative_zones),
        move |value| {
            update(GexLevelsConfig {
                max_negative_zones: value as u8,
                ..levels
            })
        },
        |value| format!("{value:.0}"),
        Some(1.0),
    );
    let persistence = labeled_slider(
        "Persistence lookback",
        5.0..=60.0,
        f32::from(levels.persistent_lookback_minutes),
        move |value| {
            update(GexLevelsConfig {
                persistent_lookback_minutes: value as u16,
                ..levels
            })
        },
        |value| format!("{value:.0} min"),
        Some(1.0),
    );
    let fade = labeled_slider(
        "Fade buckets",
        1.0..=3.0,
        f32::from(levels.fade_buckets),
        move |value| {
            update(GexLevelsConfig {
                fade_buckets: value as u8,
                ..levels
            })
        },
        |value| format!("{value:.0}"),
        Some(1.0),
    );
    let profile_width = labeled_slider(
        "Profile width",
        4.0..=7.0,
        levels.current_profile_width_percent,
        move |current_profile_width_percent| {
            update(GexLevelsConfig {
                current_profile_width_percent,
                ..levels
            })
        },
        |value| format!("{value:.0}%"),
        Some(1.0),
    );
    let settings = column![
        text("Data").size(crate::style::text_size::SECTION),
        text("Expiry filter"),
        expiry,
        text("Gamma source"),
        gamma_source,
        minimum_oi,
        minimum_gex,
        text("Zones").size(crate::style::text_size::SECTION),
        row![text("Positive"), positive_color].spacing(8),
        row![text("Negative"), negative_color].spacing(8),
        minimum_strength,
        level_match_tolerance,
        max_positive,
        max_negative,
        persistence,
        fade,
        toggle(
            "Show historical zones",
            levels.show_historical_zones,
            |c, v| c.show_historical_zones = v
        ),
        toggle(
            "Show active projection",
            levels.show_active_projection,
            |c, v| c.show_active_projection = v
        ),
        text("Profile").size(crate::style::text_size::SECTION),
        toggle(
            "Show current profile",
            levels.show_current_profile,
            |c, v| c.show_current_profile = v
        ),
        profile_width,
        text("Levels").size(crate::style::text_size::SECTION),
        toggle("Show CW", levels.show_call_wall_marker, |c, v| c
            .show_call_wall_marker =
            v),
        toggle("Show PW", levels.show_put_wall_marker, |c, v| c
            .show_put_wall_marker =
            v),
        toggle("Show GF", levels.show_gamma_flip_marker, |c, v| c
            .show_gamma_flip_marker =
            v),
        text("Interaction").size(crate::style::text_size::SECTION),
        toggle("Show tooltip", levels.show_hover_tooltip, |c, v| c
            .show_hover_tooltip =
            v),
        text(provider_status).size(crate::style::text_size::SMALL),
    ]
    .spacing(6);
    if advanced {
        sections = sections.push(settings);
    } else {
        sections = sections.push(
            column![
                text("Expiry filter"),
                pick_list(
                    GexExpiryFilter::ALL,
                    Some(levels.expiry_filter),
                    move |expiry_filter| update(GexLevelsConfig {
                        expiry_filter,
                        ..levels
                    })
                ),
                text("Gamma source"),
                pick_list(
                    GexGammaSource::ALL,
                    Some(levels.gamma_source),
                    move |gamma_source| update(GexLevelsConfig {
                        gamma_source,
                        ..levels
                    })
                ),
                toggle(
                    "Show current profile",
                    levels.show_current_profile,
                    |c, v| c.show_current_profile = v
                ),
                toggle(
                    "Show active projection",
                    levels.show_active_projection,
                    |c, v| c.show_active_projection = v
                ),
                toggle("Show call wall", levels.show_call_wall_marker, |c, v| c
                    .show_call_wall_marker =
                    v),
                toggle("Show put wall", levels.show_put_wall_marker, |c, v| c
                    .show_put_wall_marker =
                    v),
                toggle("Show gamma flip", levels.show_gamma_flip_marker, |c, v| c
                    .show_gamma_flip_marker =
                    v),
                text(provider_status).size(crate::style::text_size::SMALL)
            ]
            .spacing(8),
        );
    }

    sections.into()
}
