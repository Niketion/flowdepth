use data::layout::pane::ContentKind;
use iced::{
    Alignment, Element, Length,
    widget::{button, column, container, responsive, row, text},
};

fn label(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::CandlestickChart => "Candlestick",
        ContentKind::FootprintChart => "Footprint",
        ContentKind::ShaderHeatmap => "Heatmap",
        ContentKind::Ladder => "DOM / Ladder",
        ContentKind::TimeAndSales => "Time & Sales",
        ContentKind::GexChart => "GEX Options",
        ContentKind::ComparisonChart => "Comparison",
        ContentKind::Starter | ContentKind::HeatmapChart => unreachable!("not addable"),
    }
}

fn description(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::CandlestickChart => "Price candles over time",
        ContentKind::FootprintChart => "Order flow per candle",
        ContentKind::ShaderHeatmap => "Order book liquidity",
        ContentKind::Ladder => "Live depth ladder",
        ContentKind::TimeAndSales => "Live executed trades",
        ContentKind::GexChart => "BTC/ETH options exposure",
        ContentKind::ComparisonChart => "Compare markets",
        ContentKind::Starter | ContentKind::HeatmapChart => unreachable!("not addable"),
    }
}

fn ellipsize(value: &str, available_width: f32) -> String {
    let max_chars = ((available_width - 4.0).max(20.0) / 6.2).floor() as usize;
    let char_count = value.chars().count();
    if char_count <= max_chars {
        value.to_owned()
    } else {
        let keep = max_chars.saturating_sub(3);
        format!("{}...", value.chars().take(keep).collect::<String>())
    }
}

pub fn selector<'a, Message: Clone + 'a>(
    columns: usize,
    on_select: impl Fn(ContentKind) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    let card = |kind| {
        button(responsive(move |size| -> Element<'a, Message> {
            let description = ellipsize(description(kind), size.width);
            container(
                column![
                    text(label(kind)).size(crate::style::text_size::SECTION),
                    text(description)
                        .size(crate::style::text_size::SMALL)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .style(|theme: &iced::Theme| iced::widget::text::Style {
                            color: Some(theme.palette().background.weak.text),
                        }),
                ]
                .spacing(3)
                .width(Length::Fill),
            )
            .height(Length::Fill)
            .align_y(Alignment::Center)
            .into()
        }))
        .on_press(on_select(kind))
        .width(Length::Fill)
        .height(Length::Fixed(58.0))
        .padding(8)
        .style(crate::style::button::add_view_card)
    };

    if columns <= 1 {
        ContentKind::ADDABLE
            .into_iter()
            .fold(column![].spacing(6), |column, kind| column.push(card(kind)))
            .width(Length::Fill)
            .into()
    } else {
        let mut grid = column![].spacing(6).align_x(Alignment::Center);
        for kinds in ContentKind::ADDABLE.chunks(2) {
            let mut grid_row = row![].spacing(6).width(Length::Fill);
            for &kind in kinds {
                grid_row = grid_row.push(card(kind));
            }
            if kinds.len() == 1 {
                grid_row = grid_row.push(iced::widget::Space::new().width(Length::Fill));
            }
            grid = grid.push(grid_row);
        }
        grid.width(Length::Fill).into()
    }
}
