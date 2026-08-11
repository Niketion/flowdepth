use crate::{chart::gex, style};
use data::chart::gex::{
    GammaLiquidityRegime, GammaVegaRegime, GexFreshness, GexSignModel, GexStrike,
    IntrinsicStressLevel, OiProxyAgreement,
};
use iced::{
    Alignment, Background, Border, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse,
    widget::{
        button, canvas, column, container, mouse_area, responsive, row, rule, space, svg, text,
        tooltip,
    },
};

pub fn view(chart: &gex::GexChart) -> Element<'_, gex::Message> {
    responsive(move |size| view_sized(chart, GexLayoutDensity::for_width(size.width))).into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GexLayoutDensity {
    Full,
    Compact,
    Minimal,
}

impl GexLayoutDensity {
    fn for_width(width: f32) -> Self {
        if width >= 720.0 {
            Self::Full
        } else if width >= 520.0 {
            Self::Compact
        } else {
            Self::Minimal
        }
    }
}

fn view_sized(chart: &gex::GexChart, density: GexLayoutDensity) -> Element<'_, gex::Message> {
    let Some(snapshot) = chart.snapshot() else {
        let status_message = if chart.underlying() == exchange::options::OptionsUnderlying::Gld
            && chart.liquidity_reference().is_none()
        {
            "Select an XAU/XAUT liquidity market above to map the GLD option levels."
        } else if chart.freshness() == GexFreshness::Error {
            "GEX data unavailable."
        } else {
            "Waiting for GEX data..."
        };
        let status = text(status_message);
        let mut content = column![status]
            .spacing(12)
            .align_x(Alignment::Center)
            .width(Length::Fill);
        if chart.quantwheel_quota().is_some() {
            content = content
                .push(container(quantwheel_quota_card(chart, density)).width(Length::Fixed(420.0)));
        }
        return iced::widget::center(content).into();
    };
    let visible = chart.visible_strikes();
    if visible.is_empty() {
        return iced::widget::center(text("No strikes in the configured range.")).into();
    }

    let header = header_view(chart, snapshot, density);
    let analytics = analytics_view(chart, snapshot, density);
    let table = canvas(GexProfileTable {
        snapshot,
        strikes: visible,
        density,
        config: chart.config(),
    })
    .width(Length::Fill)
    .height(Length::Fill);
    let table = mouse_area(table)
        .on_double_click(gex::Message::AutoFit)
        .on_scroll(gex::Message::Scrolled)
        .on_press(gex::Message::DragStarted)
        .on_move(gex::Message::Dragged)
        .on_release(gex::Message::DragEnded)
        .interaction(if chart.is_dragging() {
            iced::mouse::Interaction::Grabbing
        } else {
            iced::mouse::Interaction::Grab
        });
    let padding = if density == GexLayoutDensity::Full {
        8
    } else {
        4
    };
    column![header, analytics, table]
        .spacing(4)
        .padding(padding)
        .into()
}

#[derive(Debug, Clone, Copy)]
enum Semantic {
    Primary,
    Secondary,
    Success,
    Danger,
    Warning,
}

#[derive(Debug, Clone, Copy)]
struct GaugeVisual {
    asset: &'static [u8],
    normalized: Option<f32>,
    muted: bool,
}

fn semantic_pair(theme: &Theme, semantic: Semantic) -> iced::theme::palette::Pair {
    let palette = theme.extended_palette();
    match semantic {
        Semantic::Primary => palette.primary.weak,
        Semantic::Secondary => palette.secondary.weak,
        Semantic::Success => palette.success.weak,
        Semantic::Danger => palette.danger.weak,
        Semantic::Warning => palette.warning.weak,
    }
}

fn card_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: None,
        border: Border {
            color: palette.background.strong.color.scale_alpha(0.35),
            width: 1.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

fn info<'a>(methodology: String) -> Element<'a, gex::Message> {
    tooltip(
        text("ⓘ").size(style::text_size::TINY),
        container(text(methodology).size(style::text_size::SMALL).width(360))
            .padding(8)
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
    .into()
}

fn gauge_view<'a>(
    gauge: GaugeVisual,
    semantic: Semantic,
    height: f32,
) -> Element<'a, gex::Message> {
    const GAUGE_ASPECT_RATIO: f32 = 180.0 / 108.0;

    responsive(move |available| {
        let width = (height * GAUGE_ASPECT_RATIO).min(available.width);
        let fitted_height = (width / GAUGE_ASPECT_RATIO).min(height);
        let base = svg(svg::Handle::from_memory(gauge.asset))
            .width(width)
            .height(fitted_height)
            .opacity(if gauge.muted { 0.32_f32 } else { 0.88_f32 })
            .style(|theme: &Theme, _| svg::Style {
                color: Some(theme.extended_palette().secondary.strong.color),
            });
        let needle = canvas(GaugeNeedle {
            normalized: gauge.normalized,
            semantic,
            muted: gauge.muted,
        })
        .width(width)
        .height(fitted_height);
        container(iced::widget::stack![base, needle])
            .width(Length::Fill)
            .height(height)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .into()
    })
    .width(Length::Fill)
    .height(height)
    .into()
}

#[derive(Debug, Clone, Copy)]
struct GaugeNeedle {
    normalized: Option<f32>,
    semantic: Semantic,
    muted: bool,
}

impl canvas::Program<gex::Message> for GaugeNeedle {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let Some(normalized) = self.normalized else {
            return Vec::new();
        };
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let scale = (bounds.width / 180.0).min(bounds.height / 108.0);
        let offset = Point::new(
            (bounds.width - 180.0 * scale) * 0.5,
            (bounds.height - 108.0 * scale) * 0.5,
        );
        let center = Point::new(offset.x + 90.0 * scale, offset.y + 84.0 * scale);
        let sweep = 210.0_f32.to_radians();
        let angle = 195.0_f32.to_radians() - sweep * normalized.clamp(0.0, 1.0);
        let tip = Point::new(
            center.x + 49.0 * scale * angle.cos(),
            center.y - 49.0 * scale * angle.sin(),
        );
        let color = semantic_pair(theme, self.semantic)
            .color
            .scale_alpha(if self.muted { 0.42 } else { 1.0 });
        frame.stroke(
            &canvas::Path::line(center, tip),
            canvas::Stroke::default()
                .with_color(
                    theme
                        .extended_palette()
                        .background
                        .strong
                        .color
                        .scale_alpha(0.72),
                )
                .with_width(4.2 * scale),
        );
        frame.stroke(
            &canvas::Path::line(center, tip),
            canvas::Stroke::default()
                .with_color(color)
                .with_width(2.4 * scale),
        );
        frame.fill(&canvas::Path::circle(center, 5.2 * scale), color);
        frame.fill(
            &canvas::Path::circle(center, 2.2 * scale),
            theme.extended_palette().background.weak.color,
        );
        vec![frame.into_geometry()]
    }
}

fn analytics_section<'a>(
    title: &'static str,
    gauge: GaugeVisual,
    status: String,
    semantic: Semantic,
    primary: String,
    secondary: String,
    methodology: String,
    action: Option<gex::Message>,
    density: GexLayoutDensity,
) -> Element<'a, gex::Message> {
    let status = text(status)
        .size(9)
        .wrapping(iced::widget::text::Wrapping::None)
        .style(move |theme: &Theme| iced::widget::text::Style {
            color: Some(semantic_pair(theme, semantic).color),
        });
    let heading = row![
        text(title)
            .size(style::text_size::TINY)
            .width(Length::Fill)
            .wrapping(iced::widget::text::Wrapping::None),
        status,
        info(methodology)
    ]
    .spacing(3)
    .align_y(Alignment::Center);
    let primary_text = text(primary)
        .size(style::text_size::SECTION)
        .width(Length::Fill)
        .wrapping(iced::widget::text::Wrapping::None);
    let primary: Element<_> = if let Some(message) = action {
        button(primary_text)
            .width(Length::Fill)
            .padding(0)
            .style(button::text)
            .on_press(message)
            .into()
    } else {
        primary_text.into()
    };
    let gauge_height = match density {
        GexLayoutDensity::Full => 42.0,
        GexLayoutDensity::Compact => 40.0,
        GexLayoutDensity::Minimal => 38.0,
    };
    let secondary = text(secondary)
        .size(9)
        .width(Length::Fill)
        .wrapping(iced::widget::text::Wrapping::None)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme.palette().text.scale_alpha(0.58)),
        });
    let content = column![
        heading,
        gauge_view(gauge, semantic, gauge_height),
        primary,
        secondary,
    ]
    .width(Length::Fill)
    .spacing(0);
    container(content)
        .width(Length::FillPortion(1))
        .padding([2, 6])
        .clip(true)
        .into()
}

fn analytics_view<'a>(
    chart: &'a gex::GexChart,
    snapshot: &data::chart::gex::GexSnapshot,
    density: GexLayoutDensity,
) -> Element<'a, gex::Message> {
    let cfg = chart.config();
    let expiry = cfg.expiry_filter.to_string();
    let mut sections = Vec::new();
    let is_proxy = snapshot.proxy.is_some();
    if cfg.show_intrinsic_stress_panel && !is_proxy {
        let metrics = &snapshot.intrinsic_stress;
        let semantic = match metrics.level {
            IntrinsicStressLevel::Low => Semantic::Success,
            IntrinsicStressLevel::Mild => Semantic::Warning,
            IntrinsicStressLevel::Elevated | IntrinsicStressLevel::High => Semantic::Danger,
        };
        sections.push(analytics_section(
            "Intrinsic pressure",
            GaugeVisual {
                asset: include_bytes!("../../../assets/gex/intrinsic-pressure-gauge.svg"),
                normalized: Some(normalize_intrinsic_pressure(metrics.intrinsic_ratio)),
                muted: false,
            },
            match metrics.level {
                IntrinsicStressLevel::Low => "Low",
                IntrinsicStressLevel::Mild => "Moderate",
                IntrinsicStressLevel::Elevated => "Elevated",
                IntrinsicStressLevel::High => "High",
            }
            .into(),
            semantic,
            format_unsigned_exposure(metrics.gross_intrinsic_usd),
            format!(
                "{:.1}% OI ({} / {} ITM)",
                metrics.intrinsic_ratio * 100.0,
                metrics.itm_contracts, metrics.total_contracts
            ),
            format!(
                "Calculated from open interest and intrinsic option value.\n\
                 Formula: Σ max(±(spot − strike), 0) × OI underlying; ratio = gross intrinsic USD / Σ(OI underlying × spot).\n\
                 Units: USD and ratio. Expiry filter: {expiry}. Source: Deribit option chain.\n\
                 This is an OI-based exposure proxy, not observed dealer liability. Thresholds are model thresholds, not historical percentiles."
            ),
            None,
            density,
        ));
    }
    if cfg.show_gamma_vega_panel && !is_proxy {
        let metrics = &snapshot.gamma_vega;
        let semantic = match metrics.regime {
            GammaVegaRegime::VegaDominant => Semantic::Primary,
            GammaVegaRegime::Balanced => Semantic::Warning,
            GammaVegaRegime::GammaDominant => Semantic::Success,
            GammaVegaRegime::Unavailable => Semantic::Secondary,
        };
        let regime = match metrics.regime {
            GammaVegaRegime::VegaDominant => "Vega-led",
            GammaVegaRegime::Balanced => "Balanced",
            GammaVegaRegime::GammaDominant => "Gamma-led",
            GammaVegaRegime::Unavailable => "Pending",
        };
        sections.push(analytics_section(
            "Gamma vs Vega",
            GaugeVisual {
                asset: include_bytes!("../../../assets/gex/gamma-vega-gauge.svg"),
                normalized: metrics.gamma_vega_ratio.map(normalize_gamma_vega),
                muted: metrics.gamma_vega_ratio.is_none(),
            },
            regime.into(),
            semantic,
            metrics
                .gamma_vega_ratio
                .map_or_else(|| "—".into(), format_ratio),
            format!(
                "Gamma {} / Vega {}",
                format_unsigned_exposure(metrics.gamma_shock_1pct_usd),
                format_unsigned_exposure(metrics.vega_shock_1vol_usd)
            ),
            format!(
                "Compares estimated USD exposure to a 1% spot move with exposure to a 1 vol-point IV move.\n\
                 Formula: gamma shock = absolute GEX for a 1% spot move; vega shock = Σ BS vega × OI underlying × 0.01; G/V = gamma / vega.\n\
                 Units: USD and ratio. Expiry filter: {expiry}. Source: Deribit option chain.\n\
                 Black-Scholes is a model estimate using mark IV and does not identify dealer positioning."
            ),
            None,
            density,
        ));
    }
    if cfg.show_gamma_liquidity_panel {
        sections.push(liquidity_card(chart, &expiry, density));
    }
    if is_proxy && chart.quantwheel_quota().is_some() {
        sections.push(quantwheel_quota_card(chart, density));
    }
    if !is_proxy {
        sections.push(agreement_card(chart, snapshot, &expiry, density));
    }
    cards_layout(sections, density)
}

fn quantwheel_quota_card(
    chart: &gex::GexChart,
    _density: GexLayoutDensity,
) -> Element<'_, gex::Message> {
    let quota = chart
        .quantwheel_quota()
        .expect("quota card requires QuantWheel quota state");
    let used = quota.used();
    let remaining = quota.remaining;
    let exhausted = remaining == Some(0);
    let status = match remaining {
        Some(0) => "LIMIT REACHED".to_owned(),
        Some(value) => format!("{value} LEFT"),
        None => "USAGE UNKNOWN".to_owned(),
    };
    let countdown = if exhausted {
        let remaining_ms = quota.reset_at.saturating_diff(exchange::UnixMs::now());
        let estimate = if quota.reset_is_estimate {
            "estimated · 00:00 UTC"
        } else {
            "provider reset"
        };
        Some((format_countdown(remaining_ms), estimate))
    } else {
        None
    };

    let badge_label = if quota.authenticated {
        "SIGNED IN".to_owned()
    } else {
        format!("FREE · {}/DAY", quota.limit)
    };
    let info_text = if quota.authenticated {
        "QuantWheel requests use the authenticated session stored in the Windows keychain. Account allowance and remaining usage are shown only when reported by the provider.".to_owned()
    } else {
        "QuantWheel documents 5 anonymous results per feature per day. Usage comes from the provider's x-ratelimit-remaining response header. QuantWheel does not currently return a reset timestamp, so 00:00 UTC is shown as an estimate.".to_owned()
    };
    let badge = container(text(badge_label).size(9))
        .padding([2, 6])
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                text_color: Some(palette.primary.strong.text),
                background: Some(Background::Color(palette.primary.strong.color)),
                border: Border {
                    radius: 8.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            }
        });
    let status = text(status)
        .size(9)
        .style(move |theme: &Theme| iced::widget::text::Style {
            color: Some(if exhausted {
                theme.extended_palette().danger.strong.color
            } else {
                theme.extended_palette().primary.strong.color
            }),
        });
    let heading = row![
        text("QuantWheel allowance")
            .size(style::text_size::TINY)
            .width(Length::Fill),
        badge,
        status,
        info(info_text),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let mut meter = row![].spacing(4).width(Length::Fill);
    if !quota.limit_is_estimate {
        for index in 0..quota.limit {
            let filled = used.is_some_and(|used| index < used);
            meter = meter.push(
                container(space::horizontal())
                    .height(6)
                    .width(Length::FillPortion(1))
                    .style(move |theme: &Theme| quota_segment_style(theme, filled, exhausted)),
            );
        }
    }

    let usage_value = if quota.authenticated && quota.limit_is_estimate {
        remaining
    } else {
        used
    }
    .map_or_else(|| "—".to_owned(), |value| value.to_string());
    let usage_label = if quota.authenticated && quota.limit_is_estimate {
        "account fetches remaining today"
    } else if quota.authenticated {
        "account fetches used today"
    } else {
        "anonymous fetches used today"
    };
    let mut usage = row![text(usage_value).size(26)].spacing(4);
    if !quota.limit_is_estimate {
        usage = usage.push(
            text(format!("/ {}", quota.limit))
                .size(style::text_size::SECTION)
                .style(|theme: &Theme| iced::widget::text::Style {
                    color: Some(theme.palette().text.scale_alpha(0.55)),
                }),
        );
    }
    let usage = usage
        .push(space::horizontal())
        .push(
            text(usage_label)
                .size(9)
                .style(|theme: &Theme| iced::widget::text::Style {
                    color: Some(theme.palette().text.scale_alpha(0.62)),
                }),
        )
        .align_y(Alignment::End);

    let mut content = column![heading, usage, meter]
        .spacing(7)
        .width(Length::Fill);
    if let Some((countdown, estimate)) = countdown {
        content = content.push(
            container(
                row![
                    text("NEXT FETCH").size(9),
                    space::horizontal(),
                    text(countdown).size(style::text_size::SECTION),
                    text(estimate).size(9),
                ]
                .spacing(7)
                .align_y(Alignment::Center),
            )
            .padding([5, 8])
            .style(|theme: &Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    background: Some(Background::Color(
                        palette.danger.weak.color.scale_alpha(0.24),
                    )),
                    border: Border {
                        color: palette.danger.weak.color.scale_alpha(0.5),
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..container::Style::default()
                }
            }),
        );
    }

    container(content)
        .width(Length::FillPortion(1))
        .padding([8, 10])
        .style(move |theme: &Theme| quota_card_style(theme, exhausted))
        .into()
}

fn quota_segment_style(theme: &Theme, filled: bool, exhausted: bool) -> container::Style {
    let palette = theme.extended_palette();
    let color = if filled && exhausted {
        palette.danger.strong.color
    } else if filled {
        palette.primary.strong.color
    } else {
        palette.background.strong.color.scale_alpha(0.38)
    };
    container::Style {
        background: Some(Background::Color(color)),
        border: Border {
            radius: 3.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

fn quota_card_style(theme: &Theme, exhausted: bool) -> container::Style {
    let palette = theme.extended_palette();
    let accent = if exhausted {
        palette.danger.weak.color
    } else {
        palette.primary.weak.color
    };
    container::Style {
        background: Some(Background::Color(accent.scale_alpha(0.10))),
        border: Border {
            color: accent.scale_alpha(0.55),
            width: 1.0,
            radius: 7.0.into(),
        },
        ..container::Style::default()
    }
}

fn format_countdown(milliseconds: u64) -> String {
    let total_minutes = milliseconds.saturating_add(59_999) / 60_000;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn agreement_card<'a>(
    chart: &gex::GexChart,
    snapshot: &data::chart::gex::GexSnapshot,
    expiry: &str,
    density: GexLayoutDensity,
) -> Element<'a, gex::Message> {
    let flow = chart.derive_flow();
    let agreement = data::chart::gex::oi_proxy_agreement(snapshot, flow);
    let window = flow.map(|flow| &flow.oi_proxy_comparison_30m);
    let deribit_bias = snapshot
        .net_gex_1pct
        .filter(|_| snapshot.absolute_gex_1pct > 0.0)
        .map(|net| (net / snapshot.absolute_gex_1pct).clamp(-1.0, 1.0));
    let alignment = match (agreement, deribit_bias, window) {
        (OiProxyAgreement::Agree | OiProxyAgreement::Diverge, Some(deribit), Some(derive)) => {
            Some((deribit * derive.imbalance).clamp(-1.0, 1.0))
        }
        _ => None,
    };
    let semantic = match agreement {
        OiProxyAgreement::Agree => Semantic::Success,
        OiProxyAgreement::Diverge => Semantic::Danger,
        OiProxyAgreement::Insufficient => Semantic::Secondary,
    };
    let primary = match (deribit_bias, window) {
        (Some(deribit), Some(derive)) => format!(
            "OI {:+.0}% / Flow {:+.0}%",
            deribit * 100.0,
            derive.imbalance * 100.0
        ),
        _ => "Waiting for aligned flow".into(),
    };
    let secondary = window.map_or_else(
        || format!("{expiry}: no Derive comparison data"),
        |window| {
            format!(
                "{} trades, {:.0}% matched, {}, {expiry}",
                window.trade_count,
                window.matched_deribit_gex_share * 100.0,
                window.quality
            )
        },
    );
    analytics_section(
        "OI proxy agreement (30m)",
        GaugeVisual {
            asset: include_bytes!("../../../assets/gex/oi-proxy-agreement-gauge.svg"),
            normalized: alignment.map(|score| ((score + 1.0) * 0.5) as f32),
            muted: alignment.is_none(),
        },
        agreement.to_string(),
        semantic,
        primary,
        secondary,
        format!(
            "Compares Deribit OI Proxy direction with observed settled Derive maker flow over 30 minutes.\n\
             Both sides use the same expiry and contract filters: {expiry}. The main Derive Maker Flow remains independent and includes all non-expired exact matches.\n\
             Gauge score = Deribit Net/Absolute GEX × Derive signed/gross flow; negative means divergence, positive means agreement.\n\
             Agree/Diverge requires Medium or High Derive quality and a non-balanced Deribit proxy. This is directional confirmation, not validation of true dealer GEX."
        ),
        None,
        density,
    )
}

fn liquidity_card<'a>(
    chart: &gex::GexChart,
    expiry: &str,
    density: GexLayoutDensity,
) -> Element<'a, gex::Message> {
    let methodology = format!(
        "Compares GEX exposure with visible order-book depth from the selected reference market.\n\
         Formula: effective liquidity = 2 × bid USD × ask USD / (bid USD + ask USD); impact = gamma exposure / effective liquidity.\n\
         Gamma uses |Net GEX| in OI Proxy mode and Absolute GEX otherwise. Units: USD and ratio. Expiry filter: {expiry}.\n\
         Sources: Deribit option chain plus live reference-market depth. It does not represent global BTC liquidity."
    );
    let state = chart.liquidity_depth_state();
    let reference = chart
        .liquidity_reference()
        .map(|ticker| reference_label(ticker, cfg_bps(chart)));
    if state != gex::LiquidityDepthState::Ready {
        let (status, primary, secondary, action) = match state {
            gex::LiquidityDepthState::NoReference => (
                "Setup",
                "Select market",
                "Reference market required".into(),
                Some(gex::Message::SelectLiquidityReference),
            ),
            gex::LiquidityDepthState::WaitingForDepth => (
                "Connecting",
                "Waiting for market depth",
                reference.clone().unwrap_or_default(),
                None,
            ),
            gex::LiquidityDepthState::InvalidDepth => (
                "No depth",
                "Order book not ready",
                reference.clone().unwrap_or_default(),
                None,
            ),
            gex::LiquidityDepthState::Stale => (
                "Stale",
                "Last depth is outdated",
                reference.clone().unwrap_or_default(),
                None,
            ),
            gex::LiquidityDepthState::Ready => unreachable!(),
        };
        return analytics_section(
            "Liquidity impact",
            GaugeVisual {
                asset: include_bytes!("../../../assets/gex/liquidity-impact-gauge.svg"),
                normalized: chart
                    .liquidity_metrics()
                    .map(|metrics| normalize_liquidity_impact(metrics.impact_ratio)),
                muted: true,
            },
            status.into(),
            Semantic::Secondary,
            primary.into(),
            secondary,
            methodology,
            action,
            density,
        );
    }
    let Some(metrics) = chart.liquidity_metrics() else {
        unreachable!("ready liquidity state requires metrics")
    };
    let semantic = match metrics.regime {
        GammaLiquidityRegime::LowImpact => Semantic::Success,
        GammaLiquidityRegime::Moderate => Semantic::Warning,
        GammaLiquidityRegime::Elevated | GammaLiquidityRegime::HighImpact => Semantic::Danger,
        GammaLiquidityRegime::Unavailable => Semantic::Secondary,
    };
    analytics_section(
        "Liquidity impact",
        GaugeVisual {
            asset: include_bytes!("../../../assets/gex/liquidity-impact-gauge.svg"),
            normalized: Some(normalize_liquidity_impact(metrics.impact_ratio)),
            muted: false,
        },
        metrics.regime.to_string(),
        semantic,
        format_ratio(metrics.impact_ratio),
        format!(
            "Gamma {} / Liquidity {}",
            format_unsigned_exposure(metrics.gamma_exposure_usd),
            format_unsigned_exposure(metrics.effective_liquidity_usd)
        ),
        methodology,
        None,
        density,
    )
}

fn cards_layout<'a>(
    cards: Vec<Element<'a, gex::Message>>,
    density: GexLayoutDensity,
) -> Element<'a, gex::Message> {
    let row_sizes = analytics_layout_rows(density, cards.len());
    if row_sizes.is_empty() {
        return space::vertical().height(0).into();
    }

    let mut cards = cards.into_iter();
    let mut overview = column![].width(Length::Fill).spacing(0);
    for (row_index, row_size) in row_sizes.iter().copied().enumerate() {
        let mut overview_row = row![]
            .height(Length::Shrink)
            .align_y(Alignment::Center)
            .spacing(0);
        for card_index in 0..row_size {
            if card_index > 0 {
                overview_row = overview_row.push(
                    container(rule::vertical(1.0).style(style::split_ruler))
                        .width(1)
                        .height(64)
                        .align_y(Alignment::Center),
                );
            }
            if let Some(card) = cards.next() {
                overview_row = overview_row.push(card);
            }
        }
        if row_index > 0 {
            overview = overview.push(rule::horizontal(1.0).style(style::split_ruler));
        }
        overview = overview.push(overview_row);
    }
    container(overview)
        .width(Length::Fill)
        .height(Length::Shrink)
        .padding(0)
        .style(card_style)
        .clip(true)
        .into()
}

fn analytics_layout_rows(density: GexLayoutDensity, card_count: usize) -> Vec<usize> {
    let cards_per_row = match density {
        GexLayoutDensity::Full => card_count.max(1),
        GexLayoutDensity::Compact => 2,
        GexLayoutDensity::Minimal => 1,
    };
    let mut remaining = card_count;
    let mut rows = Vec::new();
    while remaining > 0 {
        let row_size = remaining.min(cards_per_row);
        rows.push(row_size);
        remaining -= row_size;
    }
    rows
}

fn format_ratio(ratio: f64) -> String {
    if ratio >= 10.0 {
        format!("{ratio:.0}×")
    } else {
        format!("{ratio:.2}×")
    }
}

fn normalize_intrinsic_pressure(value: f64) -> f32 {
    normalize_piecewise(
        value,
        &[
            (0.0, 0.0),
            (0.02, 0.25),
            (0.05, 0.5),
            (0.10, 0.82),
            (0.20, 1.0),
        ],
    )
}

fn normalize_gamma_vega(value: f64) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        0.0
    } else {
        (0.5 + value.log10() / 6.0).clamp(0.0, 1.0) as f32
    }
}

fn normalize_liquidity_impact(value: f64) -> f32 {
    normalize_piecewise(
        value,
        &[
            (0.0, 0.0),
            (0.25, 0.33),
            (0.75, 0.66),
            (1.5, 0.88),
            (3.0, 1.0),
        ],
    )
}

fn normalize_piecewise(value: f64, points: &[(f64, f32)]) -> f32 {
    if !value.is_finite() || value <= points[0].0 {
        return points[0].1;
    }
    for window in points.windows(2) {
        let [(x0, y0), (x1, y1)] = window else {
            continue;
        };
        if value <= *x1 {
            let progress = ((value - x0) / (x1 - x0)) as f32;
            return y0 + (y1 - y0) * progress;
        }
    }
    points.last().map_or(1.0, |(_, normalized)| *normalized)
}

fn cfg_bps(chart: &gex::GexChart) -> f64 {
    f64::from(chart.config().liquidity_depth_bps)
}

fn reference_label(ticker: exchange::TickerInfo, depth_bps: f64) -> String {
    let (symbol, _) = ticker.ticker.display_symbol_and_type();
    format!("{} {symbol} ±{depth_bps:.0} bps", ticker.exchange())
}

fn format_unsigned_exposure(value: f64) -> String {
    format_exposure(value.max(0.0))
        .trim_start_matches('+')
        .to_string()
}

fn zoom_button<'a>(
    bytes: &'static [u8],
    label: &'static str,
    enabled: bool,
    message: gex::Message,
) -> Element<'a, gex::Message> {
    let icon = svg(svg::Handle::from_memory(bytes))
        .width(15)
        .height(15)
        .opacity(if enabled { 1.0_f32 } else { 0.38_f32 })
        .style(|theme: &Theme, _| svg::Style {
            color: Some(theme.palette().text),
        });
    let mut control = button(icon)
        .width(24)
        .height(24)
        .padding(4)
        .style(iced::widget::button::secondary);
    if enabled {
        control = control.on_press(message);
    }
    tooltip(
        control,
        container(text(label).size(style::text_size::SMALL))
            .padding(4)
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
    .into()
}

fn header_view<'a>(
    chart: &gex::GexChart,
    snapshot: &data::chart::gex::GexSnapshot,
    _density: GexLayoutDensity,
) -> Element<'a, gex::Message> {
    let cfg = chart.config();
    let zoom_controls = || {
        row![
            zoom_button(
                include_bytes!("../../../assets/ui/zoom-in.svg"),
                "Zoom in (double-click to auto fit)",
                chart.can_zoom_in(),
                gex::Message::ZoomIn,
            ),
            zoom_button(
                include_bytes!("../../../assets/ui/zoom-out.svg"),
                "Zoom out (double-click to auto fit)",
                chart.can_zoom_out(),
                gex::Message::ZoomOut,
            )
        ]
        .spacing(3)
    };
    if !cfg.show_summary {
        return row![space::horizontal(), zoom_controls()]
            .align_y(Alignment::Center)
            .into();
    }
    let status = match chart.freshness() {
        GexFreshness::Loading => "Loading",
        GexFreshness::Fresh => "Fresh",
        GexFreshness::Stale => "Stale",
        GexFreshness::Expired => "Expired",
        GexFreshness::Error => "Error",
    };
    let abnormal = chart.freshness() != GexFreshness::Fresh;
    let mut fields: Vec<Element<'a, gex::Message>> = Vec::new();
    let mut push = |label: &'static str, value: String| {
        fields.push(
            container(
                row![
                    text(label).size(style::text_size::SMALL),
                    text(value)
                        .size(style::text_size::SMALL)
                        .wrapping(iced::widget::text::Wrapping::None)
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            )
            .padding([2, 5])
            .style(container::rounded_box)
            .into(),
        );
    };
    if cfg.show_header_net_gex {
        push(
            "Net",
            snapshot
                .net_gex_1pct
                .map(format_exposure)
                .unwrap_or_else(|| "N/A".into()),
        );
    }
    if cfg.show_header_absolute_gex {
        push("Abs", format_exposure(snapshot.absolute_gex_1pct));
    }
    if cfg.show_header_gamma_flip {
        push("GF", format_level(snapshot.gamma_flip));
    }
    if cfg.show_header_call_wall {
        push("CW", format_level(snapshot.call_wall));
    }
    if cfg.show_header_put_wall {
        push("PW", format_level(snapshot.put_wall));
    }
    if cfg.show_header_model {
        push(
            "Model",
            if snapshot.model == GexSignModel::CallPutOiProxy {
                "OI Proxy".into()
            } else {
                "Absolute".into()
            },
        );
    }
    if cfg.show_header_expiry {
        push("Expiry", cfg.expiry_filter.to_string());
    }
    if let Some(proxy) = &snapshot.proxy {
        push(
            "Source",
            format!("{} QuantWheel proxy", proxy.source_symbol),
        );
        push("Target", proxy.target_symbol.clone());
    }
    if cfg.show_header_freshness {
        push("●", status.into());
    }
    if cfg.show_header_derive_flow && snapshot.proxy.is_none() {
        if let Some(flow) = chart.derive_flow() {
            let window = &flow.thirty_minutes;
            push(
                "Derive flow 30m",
                format!("{} / {:+.0}%", window.direction, window.imbalance * 100.0),
            );
            push(
                "Derive quality",
                format!(
                    "{} trades, {:.0}% matched, {}",
                    window.trade_count,
                    window.matched_deribit_gex_share * 100.0,
                    window.quality
                ),
            );
        } else {
            push("Derive flow 30m", "Unavailable".into());
        }
    }
    if cfg.show_header_snapshot || abnormal {
        push(
            "At",
            snapshot
                .observed_at
                .format_utc("%H:%M:%S")
                .unwrap_or_else(|| "unknown".into()),
        );
    }
    if abnormal && let Some(error) = chart.error() {
        push("!", error.to_string());
    }
    row![
        row(fields).spacing(4).align_y(Alignment::Center),
        space::horizontal(),
        zoom_controls()
    ]
    .align_y(Alignment::Center)
    .into()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct GexTableColumns {
    put_bounds: Rectangle,
    zero_x: f32,
    call_bounds: Rectangle,
    strike_bounds: Rectangle,
    net_bounds: Option<Rectangle>,
    abs_bounds: Option<Rectangle>,
    level_bounds: Rectangle,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct GexTableMetrics {
    header_height: f32,
    row_height: f32,
    bar_height: f32,
    text_size: f32,
    table_bottom: f32,
}

fn table_metrics(height: f32, visible_count: usize) -> GexTableMetrics {
    let height = height.max(1.0);
    let header_height = 22.0_f32.min(height * 0.25);
    let available_height = (height - header_height).max(0.0);
    let row_height = if visible_count == 0 {
        0.0
    } else {
        (available_height / visible_count as f32).max(1.0)
    };
    GexTableMetrics {
        header_height,
        row_height,
        bar_height: (row_height * 0.38).clamp(2.0, 16.0),
        text_size: (7.0 + row_height * 0.08).clamp(7.0, 11.0),
        table_bottom: height,
    }
}

fn table_columns(width: f32, density: GexLayoutDensity) -> GexTableColumns {
    let usable = (width - 10.0).max(120.0);
    let (bars, strike, net, abs) = match density {
        GexLayoutDensity::Full => (0.42, 0.13, 0.13, 0.13),
        GexLayoutDensity::Compact => (0.50, 0.16, 0.15, 0.0),
        GexLayoutDensity::Minimal => (0.60, 0.20, 0.0, 0.0),
    };
    let bar_width = usable * bars;
    let half = bar_width * 0.5;
    let strike_x = bar_width;
    let strike_width = usable * strike;
    let net_width = usable * net;
    let abs_width = usable * abs;
    let level_x = strike_x + strike_width + net_width + abs_width;
    GexTableColumns {
        put_bounds: Rectangle::new(Point::ORIGIN, Size::new(half, 0.0)),
        zero_x: half,
        call_bounds: Rectangle::new(Point::new(half, 0.0), Size::new(half, 0.0)),
        strike_bounds: Rectangle::new(Point::new(strike_x, 0.0), Size::new(strike_width, 0.0)),
        net_bounds: (net > 0.0).then(|| {
            Rectangle::new(
                Point::new(strike_x + strike_width, 0.0),
                Size::new(net_width, 0.0),
            )
        }),
        abs_bounds: (abs > 0.0).then(|| {
            Rectangle::new(
                Point::new(strike_x + strike_width + net_width, 0.0),
                Size::new(abs_width, 0.0),
            )
        }),
        level_bounds: Rectangle::new(
            Point::new(level_x, 0.0),
            Size::new((usable - level_x).max(30.0), 0.0),
        ),
    }
}

struct GexProfileTable<'a> {
    snapshot: &'a data::chart::gex::GexSnapshot,
    strikes: &'a [GexStrike],
    density: GexLayoutDensity,
    config: &'a data::chart::gex::Config,
}

impl canvas::Program<gex::Message> for GexProfileTable<'_> {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        _event: &canvas::Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<canvas::Action<gex::Message>> {
        None
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let palette = theme.extended_palette();
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let columns = table_columns(bounds.width, self.density);
        let metrics = table_metrics(bounds.height, self.strikes.len());
        let max_gex = self
            .strikes
            .iter()
            .map(|s| {
                if self.snapshot.model == GexSignModel::AbsoluteGamma {
                    s.absolute_gamma_1pct
                } else {
                    s.call_gex_1pct.abs().max(s.put_gex_1pct.abs())
                }
            })
            .fold(f64::EPSILON, f64::max);
        let row_centers = (0..self.strikes.len())
            .map(|index| {
                metrics.header_height + (self.strikes.len() - index) as f32 * metrics.row_height
                    - metrics.row_height * 0.5
            })
            .collect::<Vec<_>>();
        let hovered = cursor.position_in(bounds).and_then(|point| {
            (point.y >= metrics.header_height && point.y < metrics.table_bottom)
                .then(|| ((point.y - metrics.header_height) / metrics.row_height) as usize)
                .filter(|index| *index < self.strikes.len())
                .map(|visual| self.strikes.len() - 1 - visual)
        });

        draw_table_header(&mut frame, &columns, max_gex, palette.background.base.text);
        for (index, strike) in self.strikes.iter().enumerate().rev() {
            let y = row_centers[index];
            let top = y - metrics.row_height * 0.5;
            let is_call = self.snapshot.call_wall == Some(strike.strike);
            let is_put = self.snapshot.put_wall == Some(strike.strike);
            if hovered == Some(index) || is_call || is_put {
                let color = if is_call {
                    palette.success.strong.color
                } else if is_put {
                    palette.danger.strong.color
                } else {
                    palette.background.base.text
                };
                frame.fill(
                    &canvas::Path::rectangle(
                        Point::new(0.0, top),
                        Size::new(
                            columns.level_bounds.x + columns.level_bounds.width,
                            metrics.row_height,
                        ),
                    ),
                    color.scale_alpha(if hovered == Some(index) { 0.15 } else { 0.06 }),
                );
            }
            draw_strike_row(
                &mut frame,
                &columns,
                strike,
                y,
                &metrics,
                max_gex,
                self.density,
                is_call,
                is_put,
                self.snapshot.model == GexSignModel::AbsoluteGamma,
                palette,
            );
        }
        draw_references(
            &mut frame,
            &columns,
            self.strikes,
            &row_centers,
            self.config
                .show_current_price
                .then_some(self.snapshot.source_spot),
            self.config
                .show_gamma_flip
                .then_some(self.snapshot.gamma_flip)
                .flatten(),
            self.density,
            metrics.bar_height.clamp(2.0, 4.0),
            palette,
        );
        frame.stroke(
            &canvas::Path::line(
                Point::new(0.0, metrics.header_height),
                Point::new(bounds.width, metrics.header_height),
            ),
            canvas::Stroke::default()
                .with_color(palette.background.base.text.scale_alpha(0.2))
                .with_width(1.0),
        );
        let (zero_start, zero_end) = zero_line_points(&columns, &metrics);
        frame.stroke(
            &canvas::Path::line(zero_start, zero_end),
            canvas::Stroke::default()
                .with_color(palette.background.base.text.scale_alpha(0.7))
                .with_width(1.2),
        );
        let table = frame.into_geometry();
        if let (Some(index), Some(point)) = (hovered, cursor.position_in(bounds)) {
            // Keep the hover card in its own geometry so its background and
            // text are composited after every table label and reference band.
            let mut overlay = canvas::Frame::new(renderer, bounds.size());
            draw_hover(
                &mut overlay,
                bounds.size(),
                point,
                &self.strikes[index],
                self.snapshot.source_spot,
                palette,
            );
            vec![table, overlay.into_geometry()]
        } else {
            vec![table]
        }
    }
}

fn zero_line_points(columns: &GexTableColumns, metrics: &GexTableMetrics) -> (Point, Point) {
    (
        Point::new(columns.zero_x, metrics.header_height),
        Point::new(columns.zero_x, metrics.table_bottom),
    )
}

fn draw_table_header(
    frame: &mut canvas::Frame,
    columns: &GexTableColumns,
    max_gex: f64,
    color: Color,
) {
    let (negative_x, zero_x, positive_x) = scale_label_positions(columns);
    draw_text_at(
        frame,
        &format!("−{}", compact_abs(max_gex)),
        negative_x,
        11.0,
        9.0,
        color,
        iced::alignment::Horizontal::Left,
    );
    draw_text_at(
        frame,
        "0",
        zero_x,
        11.0,
        11.0,
        color,
        iced::alignment::Horizontal::Center,
    );
    draw_text_at(
        frame,
        &format!("+{}", compact_abs(max_gex)),
        positive_x,
        11.0,
        9.0,
        color,
        iced::alignment::Horizontal::Right,
    );
    draw_cell_text(frame, "Strike", columns.strike_bounds, 11.0, color);
    if let Some(bounds) = columns.net_bounds {
        draw_cell_text(frame, "Net", bounds, 11.0, color);
    }
    if let Some(bounds) = columns.abs_bounds {
        draw_cell_text(frame, "Abs", bounds, 11.0, color);
    }
    draw_cell_text(frame, "Level", columns.level_bounds, 11.0, color);
}

fn scale_label_positions(columns: &GexTableColumns) -> (f32, f32, f32) {
    (
        columns.put_bounds.x + 3.0,
        columns.zero_x,
        columns.call_bounds.x + columns.call_bounds.width - 3.0,
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_strike_row(
    frame: &mut canvas::Frame,
    columns: &GexTableColumns,
    strike: &GexStrike,
    y: f32,
    metrics: &GexTableMetrics,
    max_gex: f64,
    density: GexLayoutDensity,
    call_wall: bool,
    put_wall: bool,
    absolute_mode: bool,
    palette: &iced::theme::palette::Extended,
) {
    let put_width = if absolute_mode {
        0.0
    } else {
        (strike.put_gex_1pct.abs() / max_gex) as f32 * columns.put_bounds.width
    };
    let call_width = (if absolute_mode {
        strike.absolute_gamma_1pct
    } else {
        strike.call_gex_1pct.abs()
    } / max_gex) as f32
        * columns.call_bounds.width;
    let bar_h = metrics.bar_height;
    frame.fill(
        &canvas::Path::rectangle(
            Point::new(columns.zero_x - put_width, y - bar_h * 0.5),
            Size::new(put_width, bar_h),
        ),
        palette.danger.strong.color,
    );
    frame.fill(
        &canvas::Path::rectangle(
            Point::new(columns.zero_x, y - bar_h * 0.5),
            Size::new(call_width, bar_h),
        ),
        palette.success.strong.color,
    );
    draw_cell_text_sized(
        frame,
        &format!("{:.2}", strike.strike),
        columns.strike_bounds,
        y,
        metrics.text_size,
        palette.background.base.text,
    );
    if let Some(bounds) = columns.net_bounds {
        draw_cell_text_sized(
            frame,
            &format_exposure(strike.net_gex_1pct),
            bounds,
            y,
            metrics.text_size,
            palette.background.base.text,
        );
    }
    if let Some(bounds) = columns.abs_bounds {
        draw_cell_text_sized(
            frame,
            &format_exposure(strike.absolute_gamma_1pct),
            bounds,
            y,
            metrics.text_size,
            palette.background.base.text,
        );
    }
    let level = match (call_wall, put_wall) {
        (true, true) => {
            if density == GexLayoutDensity::Minimal {
                "CW/PW"
            } else {
                "Call / Put Wall"
            }
        }
        (true, false) => {
            if density == GexLayoutDensity::Minimal {
                "CW"
            } else {
                "Call Wall"
            }
        }
        (false, true) => {
            if density == GexLayoutDensity::Minimal {
                "PW"
            } else {
                "Put Wall"
            }
        }
        _ => "",
    };
    draw_cell_text_sized(
        frame,
        level,
        columns.level_bounds,
        y,
        metrics.text_size,
        palette.background.base.text,
    );
}

fn reference_y_for_price(
    price: f64,
    visible_strikes: &[GexStrike],
    row_centers: &[f32],
) -> Option<f32> {
    if visible_strikes.len() != row_centers.len() || visible_strikes.is_empty() {
        return None;
    }
    if price < visible_strikes.first()?.strike || price > visible_strikes.last()?.strike {
        return None;
    }
    match visible_strikes.binary_search_by(|strike| strike.strike.total_cmp(&price)) {
        Ok(index) => Some(row_centers[index]),
        Err(upper) if upper > 0 && upper < visible_strikes.len() => {
            let lower = upper - 1;
            let low = visible_strikes[lower].strike;
            let high = visible_strikes[upper].strike;
            let fraction = ((price - low) / (high - low)) as f32;
            Some(row_centers[lower] + (row_centers[upper] - row_centers[lower]) * fraction)
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_references(
    frame: &mut canvas::Frame,
    columns: &GexTableColumns,
    strikes: &[GexStrike],
    centers: &[f32],
    spot: Option<f64>,
    flip: Option<f64>,
    density: GexLayoutDensity,
    band_height: f32,
    palette: &iced::theme::palette::Extended,
) {
    let spot_y = spot.and_then(|price| reference_y_for_price(price, strikes, centers));
    let flip_y = flip.and_then(|price| reference_y_for_price(price, strikes, centers));
    if let (Some(sy), Some(fy), Some(spot), Some(flip)) = (spot_y, flip_y, spot, flip)
        && (sy - fy).abs() < 9.0
    {
        draw_reference_band(
            frame,
            columns,
            (sy + fy) * 0.5,
            &format!(
                "{} {:.2} / {} {:.2}",
                if density == GexLayoutDensity::Minimal {
                    "S"
                } else {
                    "Spot"
                },
                spot,
                if density == GexLayoutDensity::Minimal {
                    "GF"
                } else {
                    "Gamma Flip"
                },
                flip
            ),
            palette.warning.strong.color,
            band_height,
        );
        return;
    }
    if let (Some(y), Some(price)) = (spot_y, spot) {
        draw_reference_band(
            frame,
            columns,
            y,
            &format!(
                "{} {:.2}",
                if density == GexLayoutDensity::Minimal {
                    "S"
                } else {
                    "Spot"
                },
                price
            ),
            palette.primary.strong.color,
            band_height,
        );
    }
    if let (Some(y), Some(price)) = (flip_y, flip) {
        draw_reference_band(
            frame,
            columns,
            y,
            &format!(
                "{} {:.2}",
                if density == GexLayoutDensity::Minimal {
                    "GF"
                } else {
                    "Gamma Flip"
                },
                price
            ),
            palette.warning.strong.color,
            band_height,
        );
    }
}

fn draw_reference_band(
    frame: &mut canvas::Frame,
    columns: &GexTableColumns,
    y: f32,
    label: &str,
    color: Color,
    band_height: f32,
) {
    frame.fill(
        &canvas::Path::rectangle(
            Point::new(0.0, y - band_height * 0.5),
            Size::new(
                columns.call_bounds.x + columns.call_bounds.width,
                band_height,
            ),
        ),
        color.scale_alpha(0.22),
    );
    frame.stroke(
        &canvas::Path::line(
            Point::new(0.0, y),
            Point::new(columns.call_bounds.x + columns.call_bounds.width, y),
        ),
        canvas::Stroke::default()
            .with_color(color.scale_alpha(0.8))
            .with_width(0.8),
    );
    draw_cell_text(frame, label, columns.level_bounds, y, color);
}

fn hover_bounds(cursor: Point, tooltip: Size, chart: Size) -> Rectangle {
    let gap = 12.0;
    let mut x = cursor.x + gap;
    if x + tooltip.width > chart.width {
        x = cursor.x - gap - tooltip.width;
    }
    let mut y = cursor.y + gap;
    if y + tooltip.height > chart.height {
        y = cursor.y - gap - tooltip.height;
    }
    Rectangle::new(
        Point::new(
            x.clamp(0.0, (chart.width - tooltip.width).max(0.0)),
            y.clamp(0.0, (chart.height - tooltip.height).max(0.0)),
        ),
        tooltip,
    )
}

fn draw_hover(
    frame: &mut canvas::Frame,
    chart_size: Size,
    cursor: Point,
    strike: &GexStrike,
    spot: f64,
    palette: &iced::theme::palette::Extended,
) {
    let bounds = hover_bounds(cursor, Size::new(230.0, 103.0), chart_size);
    frame.fill(
        &canvas::Path::rectangle(Point::new(bounds.x + 2.0, bounds.y + 2.0), bounds.size()),
        Color::BLACK.scale_alpha(0.24),
    );
    let background = opaque_color(palette.background.base.color);
    frame.fill(
        &canvas::Path::rectangle(bounds.position(), bounds.size()),
        background,
    );
    let distance = strike.strike - spot;
    let rows = [
        ("Strike", format!("{:.2}", strike.strike)),
        (
            "Distance",
            format!("{distance:+.2} ({:+.2}%)", distance / spot * 100.0),
        ),
        ("Call GEX", format_exposure(strike.call_gex_1pct)),
        ("Put GEX", format_exposure(strike.put_gex_1pct)),
        ("Call OI", format!("{:.2}", strike.call_open_interest)),
        ("Put OI", format!("{:.2}", strike.put_open_interest)),
    ];
    for (index, (label, value)) in rows.into_iter().enumerate() {
        let y = bounds.y + 10.0 + index as f32 * 15.0;
        draw_text_at(
            frame,
            label,
            bounds.x + 8.0,
            y,
            9.0,
            opaque_color(palette.background.base.text).scale_alpha(0.78),
            iced::alignment::Horizontal::Left,
        );
        draw_text_at(
            frame,
            &value,
            bounds.x + bounds.width - 8.0,
            y,
            9.0,
            opaque_color(palette.background.base.text),
            iced::alignment::Horizontal::Right,
        );
    }
}

fn opaque_color(color: Color) -> Color {
    Color { a: 1.0, ..color }
}

fn draw_cell_text(frame: &mut canvas::Frame, value: &str, bounds: Rectangle, y: f32, color: Color) {
    draw_cell_text_sized(frame, value, bounds, y, 9.0, color);
}

fn draw_cell_text_sized(
    frame: &mut canvas::Frame,
    value: &str,
    bounds: Rectangle,
    y: f32,
    size: f32,
    color: Color,
) {
    draw_text_at(
        frame,
        value,
        bounds.x + bounds.width * 0.5,
        y,
        size,
        color,
        iced::alignment::Horizontal::Center,
    );
}

fn draw_text_at(
    frame: &mut canvas::Frame,
    value: &str,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
    align: iced::alignment::Horizontal,
) {
    frame.fill_text(canvas::Text {
        content: value.to_string(),
        position: Point::new(x, y),
        size: iced::Pixels(size),
        color,
        align_x: align.into(),
        align_y: iced::alignment::Vertical::Center,
        font: style::AZERET_MONO,
        ..canvas::Text::default()
    });
}

fn format_level(value: Option<f64>) -> String {
    value.map_or_else(|| "N/A".into(), |value| format!("{value:.2}"))
}

fn compact_abs(value: f64) -> String {
    format_exposure(value.abs())
        .trim_start_matches('+')
        .to_string()
}

pub fn format_exposure(value: f64) -> String {
    let sign = if value >= 0.0 { "+" } else { "−" };
    let absolute = value.abs();
    if absolute >= 1_000_000_000.0 {
        format!("{sign}${:.2}B", absolute / 1_000_000_000.0)
    } else if absolute >= 1_000_000.0 {
        format!("{sign}${:.2}M", absolute / 1_000_000.0)
    } else if absolute >= 1_000.0 {
        format!("{sign}${:.2}K", absolute / 1_000.0)
    } else {
        format!("{sign}${absolute:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strike(price: f64) -> GexStrike {
        GexStrike {
            strike: price,
            call_gex_1pct: 1.0,
            put_gex_1pct: -1.0,
            net_gex_1pct: 0.0,
            absolute_gamma_1pct: 2.0,
            call_open_interest: 1.0,
            put_open_interest: 1.0,
            expiration_count: 1,
            gamma_provenance: data::chart::gex::GexGammaProvenance::Derived,
        }
    }

    #[test]
    fn shared_columns_define_header_rows_and_zero() {
        for density in [
            GexLayoutDensity::Full,
            GexLayoutDensity::Compact,
            GexLayoutDensity::Minimal,
        ] {
            let columns = table_columns(800.0, density);
            assert_eq!(columns.zero_x, columns.put_bounds.width);
            assert_eq!(columns.zero_x, columns.call_bounds.x);
            let (_, label_zero, _) = scale_label_positions(&columns);
            assert_eq!(label_zero, columns.zero_x);
            let metrics = table_metrics(500.0, 20);
            let (start, end) = zero_line_points(&columns, &metrics);
            assert_eq!(start.x, columns.zero_x);
            assert_eq!(end.x, columns.zero_x);
            assert_eq!(start.y, metrics.header_height);
            assert_eq!(end.y, 500.0);
        }
    }

    #[test]
    fn responsive_columns_are_explicit() {
        let full = table_columns(800.0, GexLayoutDensity::Full);
        assert!(full.net_bounds.is_some());
        assert!(full.abs_bounds.is_some());
        let compact = table_columns(600.0, GexLayoutDensity::Compact);
        assert!(compact.net_bounds.is_some());
        assert!(compact.abs_bounds.is_none());
        let minimal = table_columns(400.0, GexLayoutDensity::Minimal);
        assert!(minimal.net_bounds.is_none());
        assert!(minimal.abs_bounds.is_none());
    }

    #[test]
    fn hover_stays_inside_and_flips_left() {
        let chart = Size::new(500.0, 300.0);
        let right = hover_bounds(Point::new(490.0, 100.0), Size::new(250.0, 148.0), chart);
        assert!(right.x < 490.0);
        assert!(right.x >= 0.0 && right.x + right.width <= chart.width);
        let bottom = hover_bounds(Point::new(200.0, 295.0), Size::new(250.0, 148.0), chart);
        assert!(bottom.y < 295.0);
        assert!(bottom.y >= 0.0 && bottom.y + bottom.height <= chart.height);
    }

    #[test]
    fn references_interpolate_between_row_centers() {
        let strikes = vec![strike(100.0), strike(120.0), strike(160.0)];
        let centers = vec![90.0, 60.0, 30.0];
        assert_eq!(reference_y_for_price(110.0, &strikes, &centers), Some(75.0));
        assert_eq!(reference_y_for_price(140.0, &strikes, &centers), Some(45.0));
        assert_eq!(reference_y_for_price(99.0, &strikes, &centers), None);
        assert_eq!(reference_y_for_price(161.0, &strikes, &centers), None);
    }

    #[test]
    fn compact_numeric_format_has_no_line_breaks() {
        for value in [-7_190_000.0, 0.0, 31_000.0] {
            assert!(!format_exposure(value).contains('\n'));
        }
    }

    #[test]
    fn metrics_fill_height_and_resize_rows() {
        let normal = table_metrics(500.0, 20);
        let fullscreen = table_metrics(1_000.0, 20);
        assert_eq!(normal.table_bottom, 500.0);
        assert_eq!(fullscreen.table_bottom, 1_000.0);
        assert!(fullscreen.row_height > normal.row_height);
        assert!(
            (normal.header_height + normal.row_height * 20.0 - normal.table_bottom).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn zoomed_strike_count_changes_visual_row_height() {
        let zoomed_out = table_metrics(600.0, 20);
        let zoomed_in = table_metrics(600.0, 10);
        assert!(zoomed_in.row_height > zoomed_out.row_height);
        assert!(zoomed_in.bar_height >= zoomed_out.bar_height);
        assert!(zoomed_in.text_size >= zoomed_out.text_size);
    }

    #[test]
    fn hover_background_is_fully_opaque() {
        assert_eq!(opaque_color(Color::from_rgba(0.1, 0.2, 0.3, 0.2)).a, 1.0);
    }

    #[test]
    fn analytics_overview_wraps_at_compact_breakpoints() {
        assert_eq!(analytics_layout_rows(GexLayoutDensity::Full, 3), vec![3]);
        assert_eq!(
            analytics_layout_rows(GexLayoutDensity::Compact, 3),
            vec![2, 1]
        );
        assert_eq!(
            analytics_layout_rows(GexLayoutDensity::Minimal, 3),
            vec![1, 1, 1]
        );
        assert!(analytics_layout_rows(GexLayoutDensity::Full, 0).is_empty());
    }
}
