use super::Element;
use crate::{
    screen::ConfirmDialog,
    style::{self, Icon, icon_text, modal_container},
};
use iced::{
    Alignment::{self, Center},
    Color,
    Length::Fill,
    Theme, border, padding,
    widget::{
        button, column, container, image, row, scrollable, slider, space, text, tooltip::Position,
    },
};

pub mod add_view;
pub mod chart;
pub mod color_picker;
pub mod column_drag;
pub mod decorate;
pub mod drag_handle;
pub mod multi_split;
pub mod toast;

pub const PANE_CONTROL_BTN_HEIGHT: f32 = 26.0;

pub const DEFAULT_TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Compatibility wrapper for the pre-0.15 `pick_list` constructor.
pub fn pick_list<'a, T, L, V, Message>(
    options: L,
    selected: Option<V>,
    on_select: impl Fn(T) -> Message + 'a,
) -> iced::widget::PickList<'a, T, L, V, Message>
where
    T: PartialEq + Clone + ToString + 'a,
    L: std::borrow::Borrow<[T]> + 'a,
    V: std::borrow::Borrow<T> + 'a,
    Message: Clone + 'a,
{
    iced::widget::pick_list(selected, options, ToString::to_string).on_select(on_select)
}

/// A compact GIF-backed activity indicator used in pane headers.
pub fn loading_spinner<'a, Message: 'a>() -> Element<'a, Message> {
    static FRAMES: std::sync::OnceLock<Vec<image::Handle>> = std::sync::OnceLock::new();
    let frames =
        FRAMES.get_or_init(|| decode_gif_frames(include_bytes!("../assets/ui/loading-dots.gif")));
    let elapsed_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let frame = frames[(elapsed_ms / 120) as usize % frames.len()].clone();

    image(frame).width(24).height(8).into()
}

/// Large animated mark used while the application resolves its initial data set.
pub fn startup_loading_animation<'a, Message: 'a>() -> Element<'a, Message> {
    static FRAMES: std::sync::OnceLock<Vec<image::Handle>> = std::sync::OnceLock::new();
    let frames = FRAMES
        .get_or_init(|| decode_gif_frames(include_bytes!("../assets/ui/startup-loading.gif")));
    let elapsed_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let frame = frames[(elapsed_ms / 80) as usize % frames.len()].clone();

    image(frame).width(148).height(148).into()
}

fn decode_gif_frames(bytes: &[u8]) -> Vec<image::Handle> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options
        .read_info(std::io::Cursor::new(bytes))
        .expect("embedded loading GIF must be valid");
    let mut frames = Vec::new();
    while let Some(frame) = decoder
        .read_next_frame()
        .expect("embedded loading GIF frame must be valid")
    {
        frames.push(image::Handle::from_rgba(
            u32::from(frame.width),
            u32::from(frame.height),
            frame.buffer.to_vec(),
        ));
    }
    assert!(!frames.is_empty(), "embedded loading GIF has no frames");
    frames
}

pub fn tooltip<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    tooltip: Option<&'a str>,
    position: Position,
) -> Element<'a, Message> {
    tooltip_with_delay(content, tooltip, position, std::time::Duration::ZERO)
}

pub fn tooltip_with_delay<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    tooltip: Option<&'a str>,
    position: Position,
    delay: std::time::Duration,
) -> Element<'a, Message> {
    match tooltip {
        Some(tooltip) => iced::widget::tooltip(
            content,
            container(text(tooltip)).style(style::tooltip).padding(8),
            position,
        )
        .delay(delay)
        .into(),
        None => content.into(),
    }
}

pub fn scrollable_content<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    scrollable::Scrollable::with_direction(
        content,
        scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(4).scroller_width(4)),
    )
    .into()
}

pub fn confirm_dialog_container<'a, Message: 'a + Clone>(
    confirm_dialog: ConfirmDialog<Message>,
    on_cancel: Message,
) -> Element<'a, Message> {
    let dialog = confirm_dialog.message;
    let on_confirm = *confirm_dialog.on_confirm;
    let on_confirm_msg = confirm_dialog.on_confirm_btn_text;

    container(
        column![
            text(dialog).size(crate::style::text_size::SECTION),
            row![
                button(text("Cancel"))
                    .style(|theme, status| style::button::transparent(theme, status, false))
                    .on_press(on_cancel),
                button(text(on_confirm_msg.unwrap_or("Confirm".to_string()))).on_press(on_confirm),
            ]
            .spacing(8),
        ]
        .align_x(Alignment::Center)
        .spacing(16),
    )
    .padding(24)
    .style(style::dashboard_modal)
    .into()
}

pub fn classic_slider_row<'a, Message>(
    label: iced::widget::Text<'a>,
    slider: Element<'a, Message>,
    placeholder: Option<iced::widget::Text<'a>>,
) -> Element<'a, Message>
where
    Message: Clone + 'a,
{
    let slider = if let Some(placeholder) = placeholder {
        column![slider, placeholder]
            .spacing(2)
            .align_x(Alignment::Center)
    } else {
        column![slider]
    };

    container(
        row![label, slider]
            .align_y(Alignment::Center)
            .spacing(8)
            .padding(8),
    )
    .style(style::modal_container)
    .into()
}

pub fn button_with_tooltip<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    message: M,
    tooltip_text: Option<&'a str>,
    tooltip_pos: crate::TooltipPosition,
    style_fn: impl Fn(&Theme, button::Status) -> button::Style + 'static,
) -> Element<'a, M> {
    let btn = button(content).style(style_fn).on_press(message);

    if let Some(text) = tooltip_text {
        tooltip(btn, Some(text), tooltip_pos)
    } else {
        btn.into()
    }
}

pub fn dragger_row<'a, Message>(
    content: Element<'a, Message>,
    is_enabled: bool,
) -> Element<'a, Message>
where
    Message: Clone + 'a,
{
    let content = if is_enabled {
        let icon = icon_text(Icon::DragHandle, 11);
        row![icon, content,]
            .align_y(Alignment::Center)
            .spacing(2)
            .into()
    } else {
        content
    };

    container(content)
        .padding(2)
        .style(style::dragger_row_container)
        .into()
}

pub fn labeled_slider<'a, T, Message: Clone + 'static>(
    label: impl text::IntoFragment<'a>,
    range: std::ops::RangeInclusive<T>,
    current: T,
    on_change: impl Fn(T) -> Message + 'a,
    to_string: impl Fn(&T) -> String,
    step: Option<T>,
) -> Element<'a, Message>
where
    T: 'static
        + Copy
        + PartialOrd
        + Into<f64>
        + From<u8>
        + num_traits::AsPrimitive<f64>
        + num_traits::FromPrimitive,
{
    let mut slider = iced::widget::slider(range, current, on_change)
        .width(Fill)
        .height(24)
        .style(|theme: &Theme, status| {
            let palette = theme.palette();

            slider::Style {
                rail: slider::Rail {
                    backgrounds: (
                        palette.background.strong.color.into(),
                        Color::TRANSPARENT.into(),
                    ),
                    width: 24.0,
                    border: border::rounded(2),
                },
                handle: slider::Handle {
                    shape: slider::HandleShape::Rectangle {
                        width: 2,
                        border_radius: 2.0.into(),
                    },
                    background: match status {
                        iced::widget::slider::Status::Active => {
                            palette.background.strong.color.into()
                        }
                        iced::widget::slider::Status::Hovered => palette.primary.base.color.into(),
                        iced::widget::slider::Status::Dragged => palette.primary.weak.color.into(),
                    },
                    border_width: 0.0,
                    border_color: Color::TRANSPARENT,
                },
            }
        });

    if let Some(v) = step {
        slider = slider.step(v);
    }

    iced::widget::stack![
        container(slider).style(modal_container),
        row![text(label), space::horizontal(), text(to_string(&current))]
            .padding([0, 10])
            .height(Fill)
            .align_y(Center),
    ]
    .into()
}

pub fn numeric_input_box<'a, F, Message: Clone + 'static>(
    label: &'a str,
    placeholder: String,
    raw_input_buf: String,
    is_input_valid: bool,
    on_input_changed: F,
    on_submit_maybe: Option<Message>,
) -> Element<'a, Message>
where
    F: Fn(String) -> Message + 'static,
{
    let text_input_widget = iced::widget::text_input(placeholder, raw_input_buf)
        .on_input(on_input_changed)
        .on_submit_maybe(on_submit_maybe)
        .align_x(iced::Alignment::Center)
        .style(move |theme, status| style::validated_text_input(theme, status, is_input_valid));

    row![text(label), text_input_widget]
        .padding(padding::right(20).left(20))
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
}

pub fn link_group_button<'a, Message, F>(
    id: iced::widget::pane_grid::Pane,
    link_group: Option<data::layout::pane::LinkGroup>,
    on_press: F,
) -> Element<'a, Message>
where
    Message: Clone + 'static,
    F: Fn(iced::widget::pane_grid::Pane) -> Message + 'static,
{
    let is_active = link_group.is_some();

    let icon = if let Some(group) = link_group {
        text(group.to_string())
    } else {
        text("-")
    };

    button(
        icon.font(style::AZERET_MONO)
            .align_x(Alignment::Start)
            .align_y(Alignment::Center),
    )
    .style(move |theme: &Theme, status| style::button::bordered_toggle(theme, status, is_active))
    .on_press(on_press(id))
    .height(PANE_CONTROL_BTN_HEIGHT)
    .width(28)
    .into()
}

#[macro_export]
/// Creates a column with horizontal rules between each item.
///
/// # Examples
/// ```
/// split_column![
///     text("Item 1"),
///     text("Item 2"),
///     text("Item 3"),
/// ] ; spacing = 8, align_x = Alignment::Start
/// ```
///
macro_rules! split_column {
    () => {
        column![]
    };

    ($item:expr $(,)?) => {
        column![$item]
    };

    ($first:expr, $($rest:expr),+ $(,)?) => {{
        let mut col = column![$first];
        $(
            col = col.push(iced::widget::rule::horizontal(1.0).style($crate::style::split_ruler));
            col = col.push($rest);
        )+
        col
    }};

    ($($item:expr),* $(,)?; spacing = $spacing:expr) => {{
        $crate::split_column![$($item),*].spacing($spacing)
    }};

    ($($item:expr),* $(,)?; spacing = $spacing:expr, align_x = $align:expr) => {{
        $crate::split_column![$($item),*].spacing($spacing).align_x($align)
    }};
}
