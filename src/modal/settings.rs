use crate::{
    Message,
    screen::{self, dashboard},
    split_column, style, version,
    widget::{pick_list, tooltip},
};
use data::{config::theme::default_theme, sidebar};
use iced::{
    Alignment, Element,
    widget::{
        button, column, container, row, scrollable, text, tooltip::Position as TooltipPosition,
    },
};
use std::borrow::Cow;

pub struct Settings {
    pub timezone: data::UserTimezone,
    pub ui_scale_factor: data::ScaleFactor,
    pub volume_size_unit: exchange::SizeUnit,
}

impl Settings {
    pub fn view<'a>(
        &'a self,
        theme: &iced_core::Theme,
        custom_theme: Option<&iced_core::Theme>,
        sidebar: &'a dashboard::sidebar::Sidebar,
        debug_terminal_enabled: bool,
    ) -> Element<'a, Message> {
        let theme_picklist = {
            let mut themes: Vec<iced::Theme> = iced_core::Theme::ALL.to_vec();

            let default_theme = iced_core::Theme::Custom(default_theme().into());
            themes.push(default_theme);

            if let Some(custom_theme) = custom_theme {
                themes.push(custom_theme.clone());
            }

            pick_list(themes, Some(theme.clone()), |theme| {
                Message::ThemeSelected(theme)
            })
        };

        let toggle_theme_editor = button(text("Theme editor")).on_press(Message::Sidebar(
            dashboard::sidebar::Message::ToggleSidebarMenu(Some(sidebar::Menu::ThemeEditor)),
        ));

        let timezone_picklist = pick_list(
            [data::UserTimezone::Utc, data::UserTimezone::Local],
            Some(self.timezone),
            Message::SetTimezone,
        );

        let size_in_quote_ccy_checkbox = {
            let is_active = match self.volume_size_unit {
                exchange::SizeUnit::Quote => true,
                exchange::SizeUnit::Base => false,
            };

            let checkbox = iced::widget::checkbox(is_active)
                .label("Size in quote currency")
                .on_toggle(|checked| {
                    let on_dialog_confirm = Message::ApplyVolumeSizeUnit(if checked {
                        exchange::SizeUnit::Quote
                    } else {
                        exchange::SizeUnit::Base
                    });

                    let confirm_dialog = screen::ConfirmDialog::new(
                        "Changing size display currency requires application restart".to_string(),
                        Box::new(on_dialog_confirm.clone()),
                    )
                    .with_confirm_btn_text("Restart now".to_string());

                    Message::ToggleDialogModal(Some(confirm_dialog))
                });

            tooltip(
                checkbox,
                Some(
                    "Display sizes/volumes in quote currency (USD).\n- Has no effect on inverse perps or open interest\n- Multi-ticker Time & Sales always shows quote values",
                ),
                TooltipPosition::Top,
            )
        };

        let cache_metadata_checkbox = {
            let cache_enabled = sidebar.cache_enabled();

            let checkbox = tooltip(
                iced::widget::checkbox(cache_enabled)
                    .label("Cache ticker metadata")
                    .on_toggle(Message::SetMetadataCaching),
                Some("Cache ticker metadata for faster startup."),
                TooltipPosition::Top,
            );

            if !cache_enabled {
                column![row![checkbox].height(24).align_y(iced::Alignment::Center)]
            } else {
                let loading = sidebar.is_metadata_loading();

                let refresh_btn = {
                    let btn = button(crate::style::icon_text(crate::style::Icon::Refresh, 12))
                        .style(move |theme, status| {
                            style::button::modifier(theme, status, loading)
                        });
                    let btn = if loading {
                        btn
                    } else {
                        btn.on_press(Message::RefreshMetadata)
                    };

                    tooltip(
                        btn,
                        if loading { None } else { Some("Refresh now") },
                        TooltipPosition::Top,
                    )
                };

                let last_updated = {
                    let last_update = sidebar.last_metadata_update();

                    let label = if loading {
                        "Refreshing metadata…".to_string()
                    } else {
                        match last_update {
                            Some(at) => {
                                format!(
                                    "Metadata last update: {}",
                                    data::util::relative_time_label(at)
                                )
                            }
                            None => "Metadata not fetched yet".to_string(),
                        }
                    };

                    text(label)
                        .size(crate::style::text_size::SMALL)
                        .style(style::secondary_text)
                };

                column![
                    row![checkbox, refresh_btn]
                        .align_y(iced::Alignment::Center)
                        .spacing(8),
                    last_updated,
                ]
                .spacing(4)
            }
        };

        let sidebar_pos_picklist = pick_list(
            [sidebar::Position::Left, sidebar::Position::Right],
            Some(sidebar.position()),
            |pos| Message::Sidebar(dashboard::sidebar::Message::SetSidebarPosition(pos)),
        );

        let scale_factor = {
            let current_value: f32 = self.ui_scale_factor.into();

            let decrease_btn = if current_value > data::config::MIN_SCALE {
                button(text("-"))
                    .on_press(Message::ScaleFactorChanged((current_value - 0.1).into()))
            } else {
                button(text("-"))
            };

            let increase_btn = if current_value < data::config::MAX_SCALE {
                button(text("+"))
                    .on_press(Message::ScaleFactorChanged((current_value + 0.1).into()))
            } else {
                button(text("+"))
            };

            container(
                row![
                    decrease_btn,
                    text(format!("{:.0}%", current_value * 100.0))
                        .size(crate::style::text_size::SECTION),
                    increase_btn,
                ]
                .align_y(Alignment::Center)
                .spacing(8)
                .padding(4),
            )
            .style(style::modal_container)
        };

        let debug_terminal_checkbox = {
            let checkbox = iced::widget::checkbox(debug_terminal_enabled)
                .label("Debug terminal")
                .on_toggle(Message::ToggleDebugTerminal);

            tooltip(
                checkbox,
                Some("Open a popup terminal with detailed application logs"),
                TooltipPosition::Top,
            )
        };

        let open_data_folder = {
            let button = button(text("Open data folder")).on_press(Message::DataFolderRequested);

            tooltip(
                button,
                Some("Open the folder where the data & config is stored"),
                TooltipPosition::Top,
            )
        };

        let invalidate_market_data_cache = {
            let clear_button = button(text("Invalidate market-data cache")).on_press(
                Message::ToggleDialogModal(Some(
                    screen::ConfirmDialog::new(
                        "This deletes all cached market data and restarts indicator analysis. Continue?"
                            .to_string(),
                        Box::new(Message::InvalidateMarketDataCache),
                    )
                    .with_confirm_btn_text("Invalidate cache".to_string()),
                )),
            );

            tooltip(
                clear_button,
                Some(
                    "Delete cached klines, trades, open interest and bubble summaries, then fetch them again",
                ),
                TooltipPosition::Top,
            )
        };

        let version_info = {
            let (version_label, commit_label) = version::app_build_version_parts();

            let github_link_button =
                button(text(version_label).size(crate::style::text_size::EMPHASIS))
                    .padding(0)
                    .style(style::button::text_link)
                    .on_press(Message::OpenUrlRequested(Cow::Borrowed(
                        version::GITHUB_REPOSITORY_URL,
                    )));

            let github_button: Element<'_, Message> = iced::widget::tooltip(
                github_link_button,
                container(
                    row![
                        text("GitHub"),
                        style::icon_text(style::Icon::ExternalLink, 12),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                .style(style::tooltip)
                .padding(8),
                TooltipPosition::Top,
            )
            .into();

            if let (Some(commit_label), Some(commit_url)) =
                (commit_label, version::build_commit_url())
            {
                let commit_button = button(text(commit_label).size(crate::style::text_size::SMALL))
                    .padding(0)
                    .style(style::button::text_link_secondary)
                    .on_press(Message::OpenUrlRequested(Cow::Owned(commit_url)));

                column![github_button, commit_button]
                    .spacing(2)
                    .align_x(Alignment::End)
                    .into()
            } else {
                github_button
            }
        };

        let footer = column![
            container(version_info)
                .width(iced::Length::Fill)
                .align_x(Alignment::End),
        ]
        .spacing(8);

        let column_content = split_column![
            column![open_data_folder,].spacing(8),
            column![text("Sidebar position").size(crate::style::text_size::SECTION), sidebar_pos_picklist,].spacing(12),
            column![text("Time zone").size(crate::style::text_size::SECTION), timezone_picklist,].spacing(12),
            column![
                text("Market data").size(crate::style::text_size::SECTION),
                size_in_quote_ccy_checkbox,
                cache_metadata_checkbox,
                invalidate_market_data_cache,
            ].spacing(12),
            column![text("Theme").size(crate::style::text_size::SECTION), theme_picklist,].spacing(12),
            column![text("Interface scale").size(crate::style::text_size::SECTION), scale_factor,].spacing(12),
            column![
                text("Experimental").size(crate::style::text_size::SECTION),
                column![
                    debug_terminal_checkbox,
                    toggle_theme_editor
                ]
                .spacing(8),
            ]
            .spacing(12),
            footer,
            ; spacing = 16, align_x = Alignment::Start
        ];

        let content = scrollable::Scrollable::with_direction(
            column_content,
            scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(8).scroller_width(6),
            ),
        );

        container(content)
            .align_x(Alignment::Start)
            .width(iced::Length::Fit.max(240))
            .padding(24)
            .style(style::dashboard_modal)
            .into()
    }
}
