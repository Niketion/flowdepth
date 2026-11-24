use super::*;
use iced::advanced::{
    Layout, layout,
    renderer::{Headless, Renderer as _},
    widget::Tree,
};

fn snapshot(
    tracker: &NewsTracker,
    renderer: &mut iced::Renderer,
    width: u32,
    height: u32,
    name: &str,
) {
    let theme: iced::Theme = data::Theme::default().into();
    let bounds = iced::Rectangle::with_size(iced::Size::new(width as f32, height as f32));
    renderer.reset(bounds);
    let mut element = tracker.view_at(width as f32, height as f32);
    let mut tree = Tree::new(element.as_widget());
    tree.diff(element.as_widget_mut());
    let node = element.as_widget_mut().layout(
        &mut tree,
        renderer,
        &layout::Limits::new(iced::Size::ZERO, bounds.size()),
    );
    let window = iced_core::window::Headless;
    let mut bus = iced_core::shell::Bus::new();
    let mut shell = iced_core::Shell::new(&window, iced_core::shell::Waker::noop(), &mut bus);
    element.as_widget_mut().update(
        &mut tree,
        &iced::Event::Window(iced::window::Event::RedrawRequested(
            std::time::Instant::now(),
        )),
        Layout::new(&node),
        iced::mouse::Cursor::Unavailable,
        renderer,
        &mut shell,
        &bounds,
    );
    element.as_widget().draw(
        &tree,
        renderer,
        &theme,
        &iced::advanced::renderer::Style {
            text_color: theme.palette().background.base.text,
        },
        Layout::new(&node),
        iced::mouse::Cursor::Unavailable,
        &bounds,
    );
    let bytes = Headless::screenshot(
        renderer,
        iced::Size::new(width, height),
        1.0,
        theme.palette().background.base.color,
    );
    std::fs::write(
        format!("target/news-ux-improved/{name}-{width}x{height}.rgba"),
        bytes,
    )
    .unwrap();
    println!("Rendered {name} {width}x{height}");
}

#[test]
#[ignore = "requires GPU rendering and live news sources"]
fn render_news_tracker_preview() {
    for title in [
        "Bitcoin was not hacked",
        "LINK price rallies",
        "Fed cuts rates",
        "Ledger confirms unauthorized hardware implant; losses may exceed $86M",
    ] {
        let (assets, category, score, _) = classify(title, vec![]);
        println!("CLASSIFY {title:?}: assets={assets:?}, category={category}, score={score}");
    }
    std::fs::create_dir_all("target/news-ux-improved").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut renderer = runtime
        .block_on(<iced::Renderer as Headless>::new(
            iced::advanced::renderer::Settings {
                default_text_size: iced::Pixels(12.0),
                ..Default::default()
            },
            Some("wgpu"),
        ))
        .expect("headless renderer");
    iced_wgpu::graphics::text::font_system()
        .write()
        .unwrap()
        .load_font(std::borrow::Cow::Borrowed(crate::style::AZERET_MONO_BYTES));
    let batch = runtime.block_on(fetch());
    let mut tracker = NewsTracker::new(Config::default());
    tracker.update(Message::Fetched(batch));
    assert!(!tracker.articles.is_empty());
    println!("Live headlines: {}", tracker.articles.len());
    println!(
        "Computer local clock: {}",
        chrono::Local::now().to_rfc3339()
    );
    snapshot(&tracker, &mut renderer, 960, 700, "desktop");
    snapshot(&tracker, &mut renderer, 480, 420, "pane");
    // Exercise the actual automatic subscription before opening any article.
    runtime.block_on(async {
        use iced::futures::StreamExt;
        let subscription = tracker.feed_reaction_subscription(uuid::Uuid::new_v4(), None);
        let recipe = iced::advanced::subscription::into_recipes(subscription)
            .pop()
            .unwrap();
        let mut stream = recipe.stream(Box::pin(iced::futures::stream::pending()));
        for _ in 0..3 {
            let message = tokio::time::timeout(Duration::from_secs(90), stream.next())
                .await
                .unwrap()
                .unwrap();
            tracker.update(message);
        }
    });
    assert!(tracker.selected.is_none());
    assert_eq!(tracker.feed_reactions.len(), 3);
    assert!(tracker.feed_reactions.values().any(|result| {
        result
            .as_ref()
            .is_ok_and(|snapshot| reaction::summary(snapshot).is_some())
    }));
    snapshot(&tracker, &mut renderer, 480, 420, "automatic-price-feed");
    snapshot(
        &tracker,
        &mut renderer,
        320,
        400,
        "narrow-automatic-price-feed",
    );
    tracker.update(Message::Select(tracker.articles[0].url.clone()));
    let article = tracker.articles[0].clone();
    let request = reaction::Request {
        article_url: article.url,
        asset: tracker.reaction_asset.clone(),
        published: article.published,
    };
    let result = runtime
        .block_on(reaction::fetch(&request, &NewsNetwork(None), false))
        .map(Box::new);
    tracker.update(Message::ReactionResult(
        request,
        tracker.reaction_nonce,
        result,
    ));
    snapshot(&tracker, &mut renderer, 960, 700, "selected");
    snapshot(&tracker, &mut renderer, 320, 400, "narrow-selected");
    tracker.update(Message::Search("no-matching-query-ux-review".into()));
    snapshot(&tracker, &mut renderer, 480, 420, "empty-selected");
    tracker.update(Message::ResetFilters);
    tracker.update(Message::ToggleFilters);
    snapshot(&tracker, &mut renderer, 320, 400, "narrow-filters");
    tracker.update(Message::ToggleSources);
    snapshot(&tracker, &mut renderer, 320, 400, "narrow-sources");
    let now = chrono::Utc::now().timestamp();
    if let Some(article) = tracker
        .articles
        .iter()
        .find(|a| {
            now - a.published > 86_400
                && a.assets
                    .iter()
                    .any(|asset| matches!(asset.as_str(), "BTC" | "ETH"))
        })
        .cloned()
    {
        tracker.update(Message::ResetFilters);
        tracker.sources_open = false;
        tracker.filters_open = false;
        tracker.update(Message::Select(article.url.clone()));
        tracker.update(Message::ReactionAsset("ETH".into()));
        let request = reaction::Request {
            article_url: article.url,
            asset: "ETH".into(),
            published: article.published,
        };
        let result = runtime
            .block_on(reaction::fetch(&request, &NewsNetwork(None), false))
            .map(Box::new);
        tracker.update(Message::ReactionResult(
            request,
            tracker.reaction_nonce,
            result,
        ));
        snapshot(&tracker, &mut renderer, 960, 700, "historical-reaction");
        snapshot(
            &tracker,
            &mut renderer,
            320,
            400,
            "narrow-historical-reaction",
        );
        tracker.config.search = tracker
            .articles
            .iter()
            .find(|a| Some(&a.url) == tracker.selected.as_ref())
            .unwrap()
            .title
            .clone();
        tracker.selected = None;
        snapshot(&tracker, &mut renderer, 480, 420, "reaction-badges");
        snapshot(&tracker, &mut renderer, 320, 400, "narrow-reaction-badges");
    }
}
