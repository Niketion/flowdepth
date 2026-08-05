use super::tickers_table::{self, TickersTable};
use crate::{
    TooltipPosition,
    layout::SavedState,
    style::{Icon, icon_text},
    widget::button_with_tooltip,
};
use data::sidebar;

use iced::{
    Alignment, Element, Subscription, Task,
    widget::responsive,
    widget::{button, column, container, row, space, text, tooltip},
};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone)]
pub enum Message {
    ToggleSidebarMenu(Option<sidebar::Menu>),
    AddViewSelected(data::layout::pane::ContentKind),
    SetSidebarPosition(sidebar::Position),
    TickersTable(super::tickers_table::Message),
}

pub struct Sidebar {
    pub state: data::Sidebar,
    pub tickers_table: TickersTable,
}

pub enum Action {
    AddViewSelected(data::layout::pane::ContentKind),
    TickerSelected(
        exchange::TickerInfo,
        Option<data::layout::pane::ContentKind>,
    ),
    ErrorOccurred(data::InternalError),
    MenuChanged(Option<sidebar::Menu>),
}

impl Sidebar {
    pub fn new(
        state: &SavedState,
        handles: exchange::adapter::AdapterHandles,
    ) -> (Self, Task<Message>) {
        let (tickers_table, initial_fetch) =
            if let Some(settings) = state.sidebar.tickers_table.as_ref() {
                TickersTable::new_with_settings(
                    settings,
                    handles.clone(),
                    state.network.rithmic.enabled,
                )
            } else {
                TickersTable::new(handles, state.network.rithmic.enabled)
            };

        (
            Self {
                state: state.sidebar.clone(),
                tickers_table,
            },
            initial_fetch.map(Message::TickersTable),
        )
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Action>) {
        match message {
            Message::ToggleSidebarMenu(menu) => {
                if menu.is_some() {
                    self.tickers_table.is_shown = false;
                }
                let new_menu = menu.filter(|&m| !self.is_menu_active(m));
                self.set_menu(new_menu);
                return (Task::none(), Some(Action::MenuChanged(new_menu)));
            }
            Message::AddViewSelected(kind) => {
                self.set_menu(None);
                return (Task::none(), Some(Action::AddViewSelected(kind)));
            }
            Message::SetSidebarPosition(position) => {
                self.state.position = position;
            }
            Message::TickersTable(msg) => {
                if matches!(msg, super::tickers_table::Message::ToggleTable) {
                    self.set_menu(None);
                }
                let action = self.tickers_table.update(msg);

                match action {
                    Some(tickers_table::Action::TickerSelected(ticker_info, content)) => {
                        return (
                            Task::none(),
                            Some(Action::TickerSelected(ticker_info, content)),
                        );
                    }
                    Some(tickers_table::Action::Fetch(task)) => {
                        return (task.map(Message::TickersTable), None);
                    }
                    Some(tickers_table::Action::ErrorOccurred(error)) => {
                        return (Task::none(), Some(Action::ErrorOccurred(error)));
                    }
                    Some(tickers_table::Action::FocusWidget(id)) => {
                        return (iced::widget::operation::focus(id), None);
                    }
                    None => {}
                }
            }
        }

        (Task::none(), None)
    }

    pub fn view(
        &self,
        audio_volume: Option<f32>,
        connectivity: crate::market_service::ConnectivityPhase,
        connected_count: usize,
        expected_count: usize,
    ) -> Element<'_, Message> {
        let state = &self.state;

        let tooltip_position = if state.position == sidebar::Position::Left {
            TooltipPosition::Right
        } else {
            TooltipPosition::Left
        };

        let is_table_open = self.tickers_table.is_shown;

        let nav_buttons = self.nav_buttons(
            is_table_open,
            audio_volume,
            connectivity,
            connected_count,
            expected_count,
            tooltip_position,
        );

        let tickers_table = if is_table_open {
            column![responsive(move |size| self
                .tickers_table
                .view(size)
                .map(Message::TickersTable))]
            .width(200)
        } else {
            column![]
        };

        match state.position {
            sidebar::Position::Left => row![nav_buttons, tickers_table],
            sidebar::Position::Right => row![tickers_table, nav_buttons],
        }
        .spacing(if is_table_open { 8 } else { 4 })
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        self.tickers_table.subscription().map(Message::TickersTable)
    }

    fn nav_buttons(
        &self,
        is_table_open: bool,
        audio_volume: Option<f32>,
        connectivity: crate::market_service::ConnectivityPhase,
        connected_count: usize,
        expected_count: usize,
        tooltip_position: TooltipPosition,
    ) -> iced::widget::Column<'_, Message> {
        let settings_modal_button = {
            let is_active = self.is_menu_active(sidebar::Menu::Settings)
                || self.is_menu_active(sidebar::Menu::ThemeEditor);

            button_with_tooltip(
                icon_text(Icon::Cog, 14)
                    .width(26)
                    .height(26)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Settings)),
                Some("Settings"),
                tooltip_position,
                move |theme, status| crate::style::button::toolbar(theme, status, is_active),
            )
        };

        let layout_modal_button = {
            let is_active = self.is_menu_active(sidebar::Menu::Layout);

            button_with_tooltip(
                icon_text(Icon::Layout, 14)
                    .width(26)
                    .height(26)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Layout)),
                Some("Dashboard layouts"),
                tooltip_position,
                move |theme, status| crate::style::button::toolbar(theme, status, is_active),
            )
        };

        let ticker_search_button = {
            button_with_tooltip(
                icon_text(Icon::Search, 14)
                    .width(26)
                    .height(26)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
                Message::TickersTable(super::tickers_table::Message::ToggleTable),
                Some("Markets"),
                tooltip_position,
                move |theme, status| crate::style::button::toolbar(theme, status, is_table_open),
            )
        };

        let add_view_button = {
            let is_active = self.is_menu_active(sidebar::Menu::AddView);
            button_with_tooltip(
                text("+")
                    .size(24)
                    .width(26)
                    .height(26)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::AddView)),
                Some("Add view"),
                tooltip_position,
                move |theme, status| {
                    crate::style::button::toolbar_primary(theme, status, is_active)
                },
            )
        };

        let audio_btn = {
            let is_active = self.is_menu_active(sidebar::Menu::Audio);

            let icon = match audio_volume.unwrap_or(0.0) {
                v if v >= 40.0 => Icon::SpeakerHigh,
                v if v > 0.0 => Icon::SpeakerLow,
                _ => Icon::SpeakerOff,
            };

            button_with_tooltip(
                icon_text(icon, 14)
                    .width(26)
                    .height(26)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Audio)),
                Some("Audio"),
                tooltip_position,
                move |theme, status| crate::style::button::toolbar(theme, status, is_active),
            )
        };

        let connection_btn: Element<'_, Message> = {
            let is_active = self.is_menu_active(sidebar::Menu::Network);
            let state = match connectivity {
                crate::market_service::ConnectivityPhase::Online => "Online",
                crate::market_service::ConnectivityPhase::Connecting => "Connecting",
                crate::market_service::ConnectivityPhase::Offline => "Offline",
            };
            let label: iced::widget::Text<'_, iced::Theme, iced::Renderer> = text("●")
                .size(14)
                .width(26)
                .height(26)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(move |theme: &iced::Theme| iced::widget::text::Style {
                    color: Some(match connectivity {
                        crate::market_service::ConnectivityPhase::Online => {
                            theme.extended_palette().success.base.color
                        }
                        crate::market_service::ConnectivityPhase::Connecting => {
                            theme.extended_palette().warning.base.color
                        }
                        crate::market_service::ConnectivityPhase::Offline => {
                            theme.extended_palette().danger.base.color
                        }
                    }),
                });
            let btn = button(label)
                .on_press(Message::ToggleSidebarMenu(Some(sidebar::Menu::Network)))
                .style(move |theme, status| {
                    crate::style::button::toolbar(theme, status, is_active)
                });
            let details = if expected_count > 0 {
                format!("Connection status: {state} ({connected_count}/{expected_count})")
            } else {
                format!("Connection status: {state}")
            };
            tooltip(
                btn,
                container(text(details))
                    .style(crate::style::tooltip)
                    .padding(8),
                tooltip_position,
            )
            .into()
        };

        column![
            ticker_search_button,
            add_view_button,
            layout_modal_button,
            space::vertical(),
            audio_btn,
            connection_btn,
            settings_modal_button,
        ]
        .width(40)
        .spacing(4)
    }

    pub fn hide_tickers_table(&mut self) -> bool {
        let table = &mut self.tickers_table;

        if table.expand_ticker_card.is_some() {
            table.expand_ticker_card = None;
            return true;
        } else if table.is_shown {
            table.is_shown = false;
            return true;
        }

        false
    }

    pub fn is_menu_active(&self, menu: sidebar::Menu) -> bool {
        self.state.active_menu == Some(menu)
    }

    pub fn active_menu(&self) -> Option<sidebar::Menu> {
        self.state.active_menu
    }

    pub fn position(&self) -> sidebar::Position {
        self.state.position
    }

    pub fn set_menu(&mut self, menu: Option<sidebar::Menu>) {
        self.state.active_menu = menu;
    }

    pub fn sync_tickers_table_settings(&mut self) {
        let settings = &self.tickers_table.settings();
        self.state.tickers_table = Some(settings.clone());
    }

    pub fn tickers_info(&self) -> &FxHashMap<exchange::Ticker, Option<exchange::TickerInfo>> {
        &self.tickers_table.tickers_info
    }

    pub fn is_metadata_loading(&self) -> bool {
        self.tickers_table.is_metadata_loading()
    }

    pub fn metadata_loading_progress(&self) -> (usize, usize) {
        self.tickers_table.metadata_loading_progress()
    }
}
