mod reaction;
mod visual;
use visual::Icon as NewsIcon;

use crate::widget::pick_list;
use data::news::{Article, Config, asset_filter, classify, evidence};
use iced::{
    Element, Length, Subscription,
    widget::{button, checkbox, column, container, responsive, row, scrollable, text, text_input},
};
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
    time::{Duration, Instant},
};

const FEEDS: [(&str, &str); 3] = [
    (
        "CoinDesk",
        "https://www.coindesk.com/arc/outboundfeeds/rss/",
    ),
    ("Cointelegraph", "https://cointelegraph.com/rss"),
    ("Decrypt", "https://decrypt.co/feed"),
];

#[derive(Debug, Clone)]
pub struct SourceState {
    name: String,
    status: String,
    healthy: bool,
    configured: bool,
    last_success: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Batch {
    articles: Vec<Article>,
    sources: Vec<SourceState>,
    fetched: i64,
}

#[derive(Debug, Clone)]
pub enum Message {
    Fetched(Batch),
    Search(String),
    Assets(String),
    Source(String),
    Minimum(u8),
    Sort(bool),
    Pause(bool),
    Refresh,
    Select(String),
    Open(String),
    CloseDetail,
    ToggleFilters,
    ToggleSources,
    ResetFilters,
    Category(String),
    Period(String),
    Macro(bool),
    Comfortable(bool),
    AssetToggled(String),
    Scrolled(bool),
    RevealNew,
    Fetching(u64),
    FetchedGeneration(u64, Batch),
    ReactionAsset(String),
    ReactionRefresh,
    FeedReactionResult(reaction::Request, Result<Box<reaction::Snapshot>, String>),
    ReactionResult(
        reaction::Request,
        u64,
        Result<Box<reaction::Snapshot>, String>,
    ),
}

pub struct NewsTracker {
    pub config: Config,
    pub generation: u64,
    articles: Vec<Article>,
    sources: Vec<SourceState>,
    selected: Option<String>,
    paused: bool,
    fetched: Option<i64>,
    pub link_error: Option<String>,
    loading: bool,
    manual_refresh: bool,
    filters_open: bool,
    sources_open: bool,
    reading: bool,
    pending: Vec<Article>,
    pub scroll_id: iced::widget::Id,
    reaction_asset: String,
    reaction_nonce: u64,
    reaction_force: bool,
    reaction_state: reaction::State,
    reaction_history: HashMap<String, reaction::Snapshot>,
    feed_reactions: HashMap<reaction::Request, Result<Box<reaction::Snapshot>, String>>,
}

impl NewsTracker {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            generation: 0,
            articles: vec![],
            sources: vec![],
            selected: None,
            paused: false,
            fetched: None,
            link_error: None,
            loading: true,
            manual_refresh: false,
            filters_open: false,
            sources_open: false,
            reading: false,
            pending: vec![],
            scroll_id: iced::widget::Id::unique(),
            reaction_asset: "BTC".into(),
            reaction_nonce: 0,
            reaction_force: false,
            reaction_state: reaction::State::Idle,
            reaction_history: HashMap::new(),
            feed_reactions: HashMap::new(),
        }
    }

    fn merge(&mut self, articles: Vec<Article>) {
        let incoming: HashSet<_> = articles.iter().map(|a| canonical_url(&a.url)).collect();
        self.articles
            .retain(|a| !incoming.contains(&canonical_url(&a.url)));
        self.articles.extend(articles);
        self.articles
            .sort_by_key(|a| std::cmp::Reverse(a.published));
        let now = chrono::Utc::now().timestamp();
        let mut urls = HashSet::new();
        self.articles.retain(|a| {
            a.published >= now - 7 * 86400
                && a.published <= now + 300
                && urls.insert(canonical_url(&a.url))
        });
        self.articles.truncate(500);
        self.feed_reactions.retain(|request, _| {
            self.articles
                .iter()
                .any(|a| a.url == request.article_url && a.published == request.published)
        });
    }

    fn clear_hidden_selection(&mut self) {
        if self.selected.as_ref().is_some_and(|url| {
            !self
                .articles
                .iter()
                .any(|a| &a.url == url && a.matches(&self.config))
        }) {
            self.selected = None;
        }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::FetchedGeneration(generation, batch) => {
                if generation == self.generation {
                    self.update(Message::Fetched(batch));
                }
            }
            Message::Fetching(generation) => {
                if generation == self.generation {
                    self.loading = true;
                }
            }
            Message::Fetched(mut batch) => {
                if self.paused && !self.manual_refresh {
                    return;
                }
                if self.fetched.is_some() && (self.reading || self.selected.is_some()) {
                    for incoming in &batch.articles {
                        if let Some(existing) = self
                            .articles
                            .iter_mut()
                            .find(|a| canonical_url(&a.url) == canonical_url(&incoming.url))
                        {
                            existing.provider.clone_from(&incoming.provider);
                        }
                    }
                    let known: HashSet<_> = self
                        .articles
                        .iter()
                        .chain(&self.pending)
                        .map(|a| canonical_url(&a.url))
                        .collect();
                    self.pending.extend(
                        batch
                            .articles
                            .into_iter()
                            .filter(|a| !known.contains(&canonical_url(&a.url))),
                    );
                    self.pending.truncate(500);
                } else {
                    self.merge(batch.articles);
                }
                for source in &mut batch.sources {
                    if !source.healthy {
                        source.last_success = self
                            .sources
                            .iter()
                            .find(|previous| previous.name == source.name)
                            .and_then(|s| s.last_success);
                    }
                }
                self.sources = batch.sources;
                self.fetched = Some(batch.fetched);
                self.loading = false;
                self.manual_refresh = false;
                self.clear_hidden_selection();
            }
            Message::Search(s) => self.config.search = s,
            Message::Assets(s) => self.config.assets = s,
            Message::Source(s) => self.config.source = (s != "All sources").then_some(s),
            Message::Category(s) => self.config.category = (s != "All categories").then_some(s),
            Message::Period(s) => {
                self.config.lookback_hours = match s.as_str() {
                    "1 hour" => 1,
                    "24 hours" => 24,
                    _ => 0,
                }
            }
            Message::Macro(v) => self.config.exclude_macro = !v,
            Message::Comfortable(v) => self.config.comfortable = v,
            Message::AssetToggled(asset) => {
                let mut selected = asset_filter(&self.config.assets);
                if selected.contains(&asset) {
                    selected.retain(|s| s != &asset);
                } else {
                    selected.push(asset);
                }
                self.config.assets = selected.join(",");
            }
            Message::Minimum(n) => self.config.min_impact = n,
            Message::Sort(v) => self.config.sort_by_impact = v,
            Message::Pause(v) => {
                self.paused = v;
                self.generation += 1;
                self.manual_refresh = false;
                self.loading = !v;
            }
            Message::Refresh => {
                if !self.loading {
                    self.manual_refresh = true;
                    self.loading = true;
                    self.generation += 1;
                }
            }
            Message::Select(url) => {
                self.reaction_asset = self
                    .reaction_history
                    .get(&url)
                    .map(|snapshot| snapshot.asset.clone())
                    .or_else(|| {
                        self.articles
                            .iter()
                            .find(|a| a.url == url)
                            .and_then(|a| a.assets.first().cloned())
                    })
                    .unwrap_or("BTC".into());
                self.reaction_nonce += 1;
                self.reaction_force = false;
                self.reaction_state = reaction::State::Loading;
                self.selected = Some(url);
                self.link_error = None;
                self.sources_open = false;
            }
            Message::CloseDetail => self.selected = None,
            Message::ToggleFilters => self.filters_open = !self.filters_open,
            Message::ToggleSources => self.sources_open = !self.sources_open,
            Message::ResetFilters => {
                self.config = Config::default();
                self.selected = None;
            }
            Message::Scrolled(reading) => self.reading = reading,
            Message::RevealNew => {
                let pending = std::mem::take(&mut self.pending);
                self.merge(pending);
                self.reading = false;
                self.selected = None;
            }
            Message::ReactionAsset(asset) => {
                self.reaction_asset = data::news::normalize_asset(&asset);
                self.reaction_nonce += 1;
                self.reaction_force = false;
                self.reaction_state = reaction::State::Loading;
            }
            Message::ReactionRefresh => {
                self.reaction_nonce += 1;
                self.reaction_force = true;
                self.reaction_state = reaction::State::Loading;
            }
            Message::FeedReactionResult(request, result) => {
                if self
                    .articles
                    .iter()
                    .any(|a| Self::feed_request(a) == request)
                {
                    self.feed_reactions.insert(request, result);
                }
            }
            Message::ReactionResult(request, nonce, result) => {
                let current = self
                    .selected
                    .as_ref()
                    .and_then(|url| self.articles.iter().find(|a| &a.url == url));
                if nonce == self.reaction_nonce
                    && request.asset == self.reaction_asset
                    && current.is_some_and(|a| {
                        a.url == request.article_url && a.published == request.published
                    })
                {
                    self.reaction_state = match result {
                        Ok(snapshot) => {
                            if self.reaction_history.len() >= 64
                                && let Some(oldest) = self
                                    .reaction_history
                                    .iter()
                                    .min_by_key(|(_, data)| data.published)
                                    .map(|(url, _)| url.clone())
                            {
                                self.reaction_history.remove(&oldest);
                            }
                            self.reaction_history
                                .insert(request.article_url, (*snapshot).clone());
                            reaction::State::Ready(snapshot)
                        }
                        Err(error) => reaction::State::Failed(error),
                    };
                }
            }
            Message::Open(url) => {
                if valid_url(&url) {
                    self.link_error = data::open_url(&url).err().map(|e| e.to_string());
                }
            }
        }
        self.clear_hidden_selection();
    }

    pub fn subscription(
        &self,
        id: uuid::Uuid,
        proxy: Option<exchange::proxy::Proxy>,
    ) -> Subscription<Message> {
        let news = self.feed_subscription(id, proxy.clone());
        let feed_reactions = self.feed_reaction_subscription(id, proxy.clone());
        let reaction = self
            .selected
            .as_ref()
            .and_then(|url| self.articles.iter().find(|article| &article.url == url))
            .map(|article| {
                let request = reaction::Request {
                    article_url: article.url.clone(),
                    asset: self.reaction_asset.clone(),
                    published: article.published,
                };
                Subscription::run_with(
                    (
                        id,
                        request,
                        self.reaction_nonce,
                        NewsNetwork(proxy),
                        self.reaction_force,
                    ),
                    |context| {
                        let (request, nonce, network, force) =
                            (context.1.clone(), context.2, context.3.clone(), context.4);
                        iced::futures::stream::unfold(false, move |wait| {
                            let request = request.clone();
                            let network = network.clone();
                            async move {
                                if wait {
                                    tokio::time::sleep(Duration::from_secs(60)).await;
                                }
                                let result = reaction::fetch(&request, &network, force && !wait)
                                    .await
                                    .map(Box::new);
                                Some((Message::ReactionResult(request, nonce, result), true))
                            }
                        })
                    },
                )
            })
            .unwrap_or_else(Subscription::none);
        Subscription::batch([news, reaction, feed_reactions])
    }

    fn feed_request(article: &Article) -> reaction::Request {
        reaction::Request {
            article_url: article.url.clone(),
            asset: article
                .assets
                .first()
                .cloned()
                .unwrap_or_else(|| "BTC".into()),
            published: article.published,
        }
    }

    fn feed_requests(&self) -> Vec<reaction::Request> {
        let mut articles: Vec<_> = self
            .articles
            .iter()
            .filter(|a| a.matches(&self.config))
            .collect();
        if self.config.sort_by_impact {
            articles.sort_by(|a, b| b.impact.cmp(&a.impact).then(b.published.cmp(&a.published)));
        }
        let mut titles = HashSet::new();
        articles
            .into_iter()
            .filter(|a| titles.insert(event_key(&a.title)))
            .map(Self::feed_request)
            .collect()
    }

    fn feed_reaction_subscription(
        &self,
        id: uuid::Uuid,
        proxy: Option<exchange::proxy::Proxy>,
    ) -> Subscription<Message> {
        let requests = self.feed_requests();
        if self.paused || requests.is_empty() {
            return Subscription::none();
        }
        Subscription::run_with((id, requests, NewsNetwork(proxy)), |context| {
            let requests = context.1.clone();
            let network = context.2.clone();
            iced::futures::stream::unfold(0usize, move |mut index| {
                let requests = requests.clone();
                let network = network.clone();
                async move {
                    if index == requests.len() {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        index = 0;
                    }
                    let request = requests[index].clone();
                    // Keep automatic history requests bounded and paced across the feed.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let result = reaction::fetch(&request, &network, false)
                        .await
                        .map(Box::new);
                    Some((Message::FeedReactionResult(request, result), index + 1))
                }
            })
        })
    }

    fn feed_subscription(
        &self,
        id: uuid::Uuid,
        proxy: Option<exchange::proxy::Proxy>,
    ) -> Subscription<Message> {
        if self.paused && !self.manual_refresh {
            return Subscription::none();
        }
        Subscription::run_with(
            (id, self.generation, self.paused, NewsNetwork(proxy)),
            |context| {
                let (generation, paused, network) = (context.1, context.2, context.3.clone());
                iced::futures::stream::unfold((0u8, false), move |(phase, fetched)| {
                    let network = network.clone();
                    async move {
                        match phase {
                            0 => Some((Message::Fetching(generation), (1, fetched))),
                            1 => Some((
                                Message::FetchedGeneration(
                                    generation,
                                    fetch_cached(&network, !fetched && generation > 0).await,
                                ),
                                (2, true),
                            )),
                            _ if paused => None,
                            _ => {
                                tokio::time::sleep(Duration::from_secs(120)).await;
                                Some((Message::Fetching(generation), (1, fetched)))
                            }
                        }
                    }
                })
            },
        )
    }

    fn stale(&self, article: &Article) -> bool {
        self.sources
            .iter()
            .any(|s| s.name == article.provider && s.configured && !s.healthy)
    }

    pub fn view(&self) -> Element<'_, Message> {
        responsive(move |size| self.view_at(size.width, size.height)).into()
    }

    fn view_at(&self, width: f32, height: f32) -> Element<'_, Message> {
        let palette_text = |theme: &iced::Theme| iced::widget::text::Style {
            color: Some(theme.palette().background.weak.text),
        };
        let healthy = self.sources.iter().filter(|s| s.healthy).count();
        let configured = self.sources.iter().filter(|s| s.configured).count();
        let health = if self.fetched.is_none() {
            "Connecting".into()
        } else {
            format!("{healthy}/{configured} sources")
        };
        let transport = row![
            visual::icon(NewsIcon::Clock, 14),
            text(if self.loading {
                "Updating..."
            } else if self.paused {
                "Paused"
            } else {
                "Auto 120s"
            })
            .size(11),
            visual::action(
                NewsIcon::Refresh,
                "Refresh news",
                (!self.loading).then_some(Message::Refresh),
                false
            ),
            visual::action(
                if self.paused {
                    NewsIcon::Play
                } else {
                    NewsIcon::Pause
                },
                if self.paused {
                    "Resume automatic updates"
                } else {
                    "Pause automatic updates"
                },
                Some(Message::Pause(!self.paused)),
                self.paused
            ),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let controls = row![
            transport,
            visual::named_action(NewsIcon::News, health, Message::ToggleSources)
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .wrap();
        let selected = self
            .selected
            .as_ref()
            .and_then(|url| self.articles.iter().find(|a| &a.url == url));
        let mut body = column![controls].spacing(8);
        if self.sources_open {
            let mut panel = column![
                row![
                    text("News sources").size(16),
                    visual::action(
                        NewsIcon::Back,
                        "Back to news feed",
                        Some(Message::ToggleSources),
                        false
                    )
                ]
                .spacing(8)
            ]
            .spacing(12);
            for source in &self.sources {
                let success = source
                    .last_success
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%d %b %H:%M %:z")
                            .to_string()
                    })
                    .unwrap_or("none yet".into());
                panel = panel.push(
                    column![
                        row![
                            visual::icon(NewsIcon::News, 18),
                            text(&source.name).size(14)
                        ]
                        .spacing(6)
                        .align_y(iced::Alignment::Center),
                        text(&source.status).size(12),
                        text(format!("Last success: {success}")).size(11)
                    ]
                    .spacing(4),
                );
            }
            if self.sources.is_empty() {
                panel = panel.push(text("Contacting publishers..."));
            }
            body = body.push(scrollable(panel).height(Length::Fill));
        } else if let Some(article) = selected
            && (width < 760.0 || height < 420.0)
        {
            body = body.push(self.detail(article).height(Length::Fill));
        } else {
            let search = row![
                container(visual::icon(NewsIcon::Search, 16))
                    .height(32)
                    .align_y(iced::Alignment::Center),
                text_input("Search headlines or topics", &self.config.search)
                    .on_input(Message::Search),
                visual::action(
                    NewsIcon::Filter,
                    "Show or hide news filters",
                    Some(Message::ToggleFilters),
                    self.filters_open
                ),
                visual::action(
                    NewsIcon::Reset,
                    "Reset all news filters",
                    Some(Message::ResetFilters),
                    false
                )
            ]
            .spacing(6);
            body = body.push(search);
            if self.filters_open {
                let mut sources = vec!["All sources".to_string()];
                sources.extend(self.articles.iter().map(|a| a.source.clone()));
                sources.sort();
                sources.dedup();
                let source = self.config.source.clone().unwrap_or("All sources".into());
                if !sources.contains(&source) {
                    sources.push(source.clone());
                }
                let categories = [
                    "All categories",
                    "Security",
                    "Systemic risk",
                    "Macro",
                    "Regulation",
                    "Institutional",
                    "Supply / listing",
                    "Network",
                    "Market",
                ]
                .map(str::to_owned);
                let category = self
                    .config
                    .category
                    .clone()
                    .unwrap_or("All categories".into());
                let period = match self.config.lookback_hours {
                    1 => "1 hour",
                    24 => "24 hours",
                    _ => "7 days",
                };
                let mut assets = row![].spacing(6);
                let selected_assets = asset_filter(&self.config.assets);
                for symbol in ["BTC", "ETH", "SOL", "LINK"] {
                    let active = selected_assets.iter().any(|s| s == symbol);
                    let selection_mark: Element<'_, Message> = if active {
                        visual::icon(NewsIcon::Check, 12).into()
                    } else {
                        iced::widget::space().width(12).into()
                    };
                    assets = assets.push(
                        button(
                            row![
                                visual::asset(symbol, 20),
                                text(symbol).size(11),
                                selection_mark
                            ]
                            .spacing(5)
                            .align_y(iced::Alignment::Center),
                        )
                        .padding([5, 6])
                        .on_press(Message::AssetToggled(symbol.into()))
                        .style(move |theme, status| {
                            crate::style::button::transparent(theme, status, active)
                        }),
                    );
                }
                let filters = column![
                    text_input("Watchlist: BTC, Ethereum, SOLUSDT...", &self.config.assets)
                        .on_input(Message::Assets),
                    assets.wrap(),
                    row![
                        pick_list(sources, Some(source), Message::Source).width(Length::Fill),
                        pick_list(categories, Some(category), Message::Category)
                            .width(Length::Fill)
                    ]
                    .spacing(6),
                    row![
                        text("Relevance >=").size(12),
                        pick_list(
                            [0, 25, 50, 75, 90],
                            Some(self.config.min_impact),
                            Message::Minimum
                        ),
                        pick_list(
                            ["1 hour", "24 hours", "7 days"].map(str::to_owned),
                            Some(period.to_string()),
                            Message::Period
                        )
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                    checkbox(self.config.sort_by_impact)
                        .label("Highest relevance first")
                        .on_toggle(Message::Sort),
                    checkbox(!self.config.exclude_macro)
                        .label("Include macro in watchlist")
                        .on_toggle(Message::Macro),
                    checkbox(self.config.comfortable)
                        .label("Expanded rows")
                        .on_toggle(Message::Comfortable),
                ]
                .spacing(6);
                body = body.push(
                    scrollable(filters).height(Length::Fixed((height * 0.35).clamp(70.0, 185.0))),
                );
            }
            let mut visible: Vec<_> = self
                .articles
                .iter()
                .filter(|a| a.matches(&self.config))
                .collect();
            if self.config.sort_by_impact {
                visible.sort_by(|a, b| b.impact.cmp(&a.impact).then(b.published.cmp(&a.published)));
            }
            let mut event_sources: HashMap<String, HashSet<&str>> = HashMap::new();
            for article in &self.articles {
                event_sources
                    .entry(event_key(&article.title))
                    .or_default()
                    .insert(&article.source);
            }
            let mut titles = HashSet::new();
            visible.retain(|a| titles.insert(event_key(&a.title)));
            let mut info = row![
                text(format!(
                    "{} events | Local time | Relevance 0-100",
                    visible.len()
                ))
                .size(11)
                .style(palette_text)
            ]
            .spacing(8);
            if !self.pending.is_empty() {
                info = info.push(visual::named_action(
                    NewsIcon::New,
                    format!("{} new", self.pending.len()),
                    Message::RevealNew,
                ));
            }
            body = body.push(info);
            let mut tape = column![].spacing(4);
            let count = visible.len();
            for article in visible {
                let timestamp = chrono::DateTime::from_timestamp(article.published, 0)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%d %b %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                let related = event_sources
                    .get(&event_key(&article.title))
                    .map_or(1, HashSet::len);
                let tag = if self.stale(article) {
                    "STALE"
                } else if evidence(&article.title) == "Unconfirmed" {
                    "RUMOR"
                } else {
                    ""
                };
                let mut badges = row![
                    badge(article.source.clone(), BadgeKind::Source),
                    badge(article.category.clone(), BadgeKind::Category)
                ]
                .spacing(5);
                for asset in &article.assets {
                    badges = badges.push(badge(asset.clone(), BadgeKind::Asset));
                }
                if related > 1 {
                    badges = badges.push(badge(format!("{related} sources"), BadgeKind::Neutral));
                }
                if !tag.is_empty() {
                    badges = badges.push(badge(tag.to_string(), BadgeKind::Warning));
                }
                let request = Self::feed_request(article);
                let snapshot = self
                    .feed_reactions
                    .get(&request)
                    .and_then(|result| result.as_ref().ok())
                    .map(Box::as_ref)
                    .or_else(|| self.reaction_history.get(&article.url));
                if let Some(snapshot) = snapshot
                    && let Some(summary) = reaction::summary(snapshot)
                {
                    let value = [1, 0, 2]
                        .into_iter()
                        .find_map(|index| match snapshot.response.readings[index] {
                            data::news::reaction::Reading::Observed(value) => Some(value),
                            _ => None,
                        })
                        .unwrap_or(0.0);
                    badges = badges.push(badge(
                        format!(
                            "{summary}{}",
                            if article.assets.contains(&snapshot.asset) {
                                ""
                            } else {
                                " · market"
                            }
                        ),
                        BadgeKind::Response(value),
                    ));
                } else {
                    let state = if self
                        .feed_reactions
                        .get(&request)
                        .is_some_and(Result::is_err)
                    {
                        "unavailable"
                    } else if snapshot.is_some() {
                        "+5m pending"
                    } else if self.paused {
                        "paused"
                    } else {
                        "loading"
                    };
                    badges = badges.push(badge(
                        format!("{} · price {state}", request.asset),
                        BadgeKind::Neutral,
                    ));
                }
                let headline: Element<_> = row![
                    container(
                        column![text("REL").size(9), relevance(article.impact)]
                            .spacing(3)
                            .align_x(iced::Alignment::Center)
                    )
                    .width(38)
                    .padding([5, 0]),
                    column![
                        text(&article.title).size(if self.config.comfortable { 14 } else { 13 }),
                        badges.wrap(),
                        row![
                            visual::icon(NewsIcon::Clock, 11),
                            text(timestamp)
                                .size(10)
                                .font(crate::style::AZERET_MONO)
                                .style(palette_text)
                        ]
                        .spacing(4)
                        .align_y(iced::Alignment::Center)
                    ]
                    .spacing(if self.config.comfortable { 6 } else { 4 })
                    .width(Length::Fill)
                ]
                .spacing(8)
                .align_y(iced::Alignment::Start)
                .into();
                tape = tape.push(
                    button(headline)
                        .padding(if self.config.comfortable { 9 } else { 6 })
                        .width(Length::Fill)
                        .on_press(Message::Select(article.url.clone()))
                        .style(move |theme, status| {
                            crate::style::button::transparent(
                                theme,
                                status,
                                self.selected.as_ref() == Some(&article.url),
                            )
                        }),
                );
            }
            if count == 0 {
                let message = if self.loading && self.fetched.is_none() {
                    "Loading headlines..."
                } else if configured > 0 && healthy == 0 {
                    "Sources unavailable. Open source status or retry."
                } else if self.articles.is_empty() {
                    "No recent headlines received."
                } else {
                    "No events match these filters."
                };
                tape = tape.push(
                    column![
                        text(message).size(13),
                        visual::named_action(
                            NewsIcon::Reset,
                            "Reset filters".into(),
                            Message::ResetFilters
                        )
                    ]
                    .spacing(8),
                );
            }
            let list: Element<_> = scrollable(tape)
                .id(self.scroll_id.clone())
                .on_scroll(|viewport| Message::Scrolled(viewport.relative_offset().y > 0.01))
                .height(Length::Fill)
                .into();
            if let Some(article) = selected {
                body = body.push(
                    row![
                        container(list).width(Length::FillPortion(3)),
                        self.detail(article).width(Length::FillPortion(2))
                    ]
                    .spacing(10)
                    .height(Length::Fill),
                );
            } else {
                body = body.push(list);
            }
        }
        if let Some(error) = &self.link_error {
            body = body.push(text(error).size(11));
        }
        container(body)
            .padding(10)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn detail<'a>(&'a self, article: &'a Article) -> iced::widget::Container<'a, Message> {
        let mut content = column![
            text(&article.title).size(16),
            row![
                relevance(article.impact),
                text(format!("Relevance | {}", article.category)).size(12),
                visual::named_action(
                    NewsIcon::Link,
                    "Source".into(),
                    Message::Open(article.url.clone())
                )
            ]
            .spacing(8),
            text(if article.assets.is_empty() {
                evidence(&article.title).to_string()
            } else {
                format!(
                    "{} | {}",
                    article.assets.join(" / "),
                    evidence(&article.title)
                )
            })
            .size(12),
        ]
        .spacing(10);
        let mut assets = vec!["BTC".to_string(), "ETH".to_string(), "SOL".to_string()];
        assets.extend(article.assets.iter().cloned());
        assets.push(self.reaction_asset.clone());
        assets.sort();
        assets.dedup();
        content = content.push(
            column![
                text("Observed market response").size(14),
                row![
                    pick_list(
                        assets,
                        Some(self.reaction_asset.clone()),
                        Message::ReactionAsset
                    ),
                    visual::action(
                        NewsIcon::Refresh,
                        "Reload historical prices",
                        (!matches!(self.reaction_state, reaction::State::Loading))
                            .then_some(Message::ReactionRefresh),
                        false
                    )
                ]
                .spacing(8),
            ]
            .spacing(6),
        );
        if !article.assets.contains(&self.reaction_asset) {
            content =
                content.push(text("Comparison asset; not identified in this headline.").size(11));
        }
        content = match &self.reaction_state {
            reaction::State::Idle | reaction::State::Loading => {
                content.push(text("Loading historical prices...").size(12))
            }
            reaction::State::Failed(error) => content.push(text(error).size(12)),
            reaction::State::Ready(snapshot) => content.push(reaction::view(snapshot)),
        };
        content = content.push(text("About the relevance score").size(12)).push(text(&article.reason).size(12)).push(text("Editorial relevance is separate from observed returns and does not express a probability.").size(11));
        if self.stale(article) {
            content =
                content.push(text("Source unavailable; this headline may be stale.").size(12));
        }
        for related in self
            .articles
            .iter()
            .filter(|a| event_key(&a.title) == event_key(&article.title))
        {
            let domain = url::Url::parse(&related.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            content = content.push(visual::named_action(
                NewsIcon::Link,
                format!("{} · {domain}", related.source),
                Message::Open(related.url.clone()),
            ));
        }
        container(
            column![
                visual::named_action(NewsIcon::Back, "Back to feed".into(), Message::CloseDetail),
                scrollable(content).height(Length::Fill)
            ]
            .spacing(10)
            .height(Length::Fill),
        )
        .padding(10)
        .height(Length::Fill)
    }
}

#[derive(Clone, Copy)]
enum BadgeKind {
    Source,
    Category,
    Asset,
    Warning,
    Neutral,
    Response(f64),
}

fn badge_color(theme: &iced::Theme, kind: BadgeKind) -> iced::Color {
    let palette = theme.palette();
    match kind {
        BadgeKind::Warning => palette.warning.base.color,
        BadgeKind::Response(value) if value > 0.0 => palette.success.base.color,
        BadgeKind::Response(value) if value < 0.0 => palette.danger.base.color,
        BadgeKind::Category => palette.background.weak.text,
        _ => palette.background.base.text,
    }
}

fn badge(label: String, kind: BadgeKind) -> Element<'static, Message> {
    let picture = match kind {
        BadgeKind::Asset => visual::asset(&label, 16),
        _ => {
            let symbol = match kind {
                BadgeKind::Source => NewsIcon::News,
                BadgeKind::Category => visual::category(&label),
                BadgeKind::Warning => NewsIcon::Warning,
                BadgeKind::Response(value) if value > 0.0 => NewsIcon::Up,
                BadgeKind::Response(value) if value < 0.0 => NewsIcon::Down,
                BadgeKind::Response(_) => NewsIcon::Flat,
                _ => NewsIcon::Clock,
            };
            visual::icon(symbol, 14)
                .style(move |theme: &iced::Theme, _| iced::widget::svg::Style {
                    color: Some(badge_color(theme, kind)),
                })
                .into()
        }
    };
    container(
        row![picture, text(label).size(10)]
            .spacing(4)
            .align_y(iced::Alignment::Center),
    )
    .padding([3, 5])
    .style(move |theme: &iced::Theme| {
        let color = badge_color(theme, kind);
        iced::widget::container::Style {
            text_color: Some(color),
            background: matches!(kind, BadgeKind::Response(_) | BadgeKind::Warning)
                .then_some(color.scale_alpha(0.14).into()),
            border: iced::Border {
                radius: 5.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

fn relevance(score: u8) -> iced::widget::Text<'static> {
    text(format!("{score}"))
        .size(12)
        .font(crate::style::AZERET_MONO)
        .style(move |theme: &iced::Theme| iced::widget::text::Style {
            color: Some(if score >= 75 {
                theme.palette().warning.base.color
            } else {
                theme.palette().background.base.text
            }),
        })
}

fn event_key(title: &str) -> String {
    title
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn valid_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| {
        matches!(u.scheme(), "https" | "http")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

fn canonical_url(value: &str) -> String {
    if let Ok(mut url) = url::Url::parse(value) {
        url.set_fragment(None);
        let query: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !k.starts_with("utm_") && k != "fbclid")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.set_query(None);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        return url.to_string();
    }
    value.into()
}

#[derive(Clone)]
struct NewsNetwork(Option<exchange::proxy::Proxy>);
impl std::hash::Hash for NewsNetwork {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(
            &self
                .0
                .as_ref()
                .map(|proxy| serde_json::to_string(proxy).unwrap_or_default()),
            state,
        );
    }
}

async fn fetch_cached(network: &NewsNetwork, force: bool) -> Batch {
    type Cache = Option<(u64, Instant, Batch)>;
    static CACHE: OnceLock<tokio::sync::Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(network, &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    if !force
        && let Some((cached_key, fetched, batch)) = &*cache
        && *cached_key == key
        && fetched.elapsed() < Duration::from_secs(120)
    {
        return batch.clone();
    }
    let batch = fetch_with_proxy(network.0.as_ref()).await;
    *cache = Some((key, Instant::now(), batch.clone()));
    batch
}

#[cfg(test)]
async fn fetch() -> Batch {
    fetch_with_proxy(None).await
}

async fn fetch_with_proxy(proxy: Option<&exchange::proxy::Proxy>) -> Batch {
    let mut batch = Batch {
        articles: vec![],
        sources: vec![],
        fetched: chrono::Utc::now().timestamp(),
    };
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Flowsurface NewsTracker/1.0");
    let Ok(client) = exchange::adapter::proxy::try_apply_proxy(builder, proxy).build() else {
        batch.sources.push(SourceState {
            name: "All sources".into(),
            status: "HTTP client initialization failed".into(),
            healthy: false,
            configured: true,
            last_success: None,
        });
        return batch;
    };
    let results = iced::futures::future::join_all(FEEDS.iter().map(|(name, url)| async {
        let result = async {
            let response = client
                .get(*url)
                .send()
                .await
                .map_err(|_| "network error")?
                .error_for_status()
                .map_err(|_| "HTTP error")?;
            let bytes = read_capped(response).await?;
            parse_rss(&bytes, name).map_err(|_| "invalid RSS")
        }
        .await;
        (*name, result)
    }))
    .await;
    for (name, result) in results {
        let status = match result {
            Ok(articles) => {
                let count = articles.len();
                batch.articles.extend(articles);
                if count == 0 {
                    "no usable headlines".into()
                } else {
                    format!("OK ({count})")
                }
            }
            Err(error) => format!("{error} · retained headlines may be stale"),
        };
        batch.sources.push(SourceState {
            name: name.into(),
            healthy: status.starts_with("OK") || status == "no usable headlines",
            configured: true,
            last_success: (status.starts_with("OK") || status == "no usable headlines")
                .then(|| chrono::Utc::now().timestamp()),
            status,
        });
    }
    let token = std::env::var("CRYPTOPANIC_AUTH_TOKEN").unwrap_or_default();
    let status = if token.trim().is_empty() {
        "Not configured. Add your API token before starting the app.".into()
    } else {
        match fetch_cryptopanic(&client, &token).await {
            Ok(articles) => {
                let count = articles.len();
                batch.articles.extend(articles);
                format!("OK ({count})")
            }
            Err(error) => format!("{error} · retained headlines may be stale"),
        }
    };
    batch.sources.push(SourceState {
        name: "CryptoPanic".into(),
        healthy: status.starts_with("OK"),
        configured: !token.trim().is_empty(),
        last_success: status
            .starts_with("OK")
            .then(|| chrono::Utc::now().timestamp()),
        status,
    });
    batch.fetched = chrono::Utc::now().timestamp();
    batch
}

async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "read error")? {
        if bytes.len() + chunk.len() > 2_000_000 {
            return Err("feed too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn fetch_cryptopanic(
    client: &reqwest::Client,
    token: &str,
) -> Result<Vec<Article>, &'static str> {
    let plan = std::env::var("CRYPTOPANIC_API_PLAN").unwrap_or("growth".into());
    if !matches!(plan.as_str(), "growth" | "growth_weekly" | "enterprise") {
        return Err("invalid API plan");
    }
    let response = client
        .get(format!("https://cryptopanic.com/api/{plan}/v2/posts/"))
        .query(&[("auth_token", token), ("public", "true"), ("kind", "news")])
        .send()
        .await
        .map_err(|_| "network error")?;
    // Never format reqwest errors: the request URL contains the secret token.
    if !response.status().is_success() {
        return Err("HTTP error: check API token / plan / quota");
    }
    let bytes = read_capped(response).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "invalid JSON")?;
    parse_cryptopanic(&value)
}

fn parse_cryptopanic(value: &serde_json::Value) -> Result<Vec<Article>, &'static str> {
    let posts = value["results"].as_array().ok_or("missing results")?;
    Ok(posts
        .iter()
        .filter_map(|post| {
            let title = post["title"].as_str()?;
            let url = post["original_url"]
                .as_str()
                .filter(|url| valid_url(url))
                .or_else(|| post["url"].as_str().filter(|url| valid_url(url)))?;
            let published = chrono::DateTime::parse_from_rfc3339(post["published_at"].as_str()?)
                .ok()?
                .timestamp();
            let symbols = post["instruments"]
                .as_array()
                .or_else(|| post["currencies"].as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|v| {
                            v["code"]
                                .as_str()
                                .or_else(|| v["symbol"].as_str())
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let (assets, category, impact, reason) = classify(title, symbols);
            let source = post["source"]["title"].as_str().unwrap_or("CryptoPanic");
            Some(Article {
                title: title.into(),
                url: url.into(),
                source: source.into(),
                provider: "CryptoPanic".into(),
                published,
                assets,
                category,
                impact,
                reason,
            })
        })
        .take(100)
        .collect())
}

fn parse_rss(bytes: &[u8], source: &str) -> Result<Vec<Article>, quick_xml::Error> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut articles = Vec::new();
    let mut in_item = false;
    let mut field = String::new();
    let (mut title, mut link, mut date) = (String::new(), String::new(), String::new());
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "item" {
                    in_item = true;
                    title.clear();
                    link.clear();
                    date.clear();
                }
                field = name;
            }
            Event::Text(e) if in_item => {
                let value = e.xml_content()?.into_owned();
                match field.as_str() {
                    "title" => title.push_str(&value),
                    "link" => link.push_str(&value),
                    "pubDate" => date.push_str(&value),
                    _ => {}
                }
            }
            Event::CData(e) if in_item => {
                let value = e.xml_content()?.into_owned();
                match field.as_str() {
                    "title" => title.push_str(&value),
                    "link" => link.push_str(&value),
                    "pubDate" => date.push_str(&value),
                    _ => {}
                }
            }
            Event::GeneralRef(e) if in_item => {
                let name = e.decode()?;
                let resolved = e.resolve_char_ref()?.or_else(|| match name.as_ref() {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => None,
                });
                if let Some(c) = resolved {
                    match field.as_str() {
                        "title" => title.push(c),
                        "link" => link.push(c),
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                if e.local_name().as_ref() == b"item" {
                    if !title.trim().is_empty()
                        && valid_url(link.trim())
                        && let Ok(published) = chrono::DateTime::parse_from_rfc2822(date.trim())
                            .or_else(|_| chrono::DateTime::parse_from_rfc3339(date.trim()))
                    {
                        let (assets, category, impact, reason) = classify(&title, vec![]);
                        articles.push(Article {
                            title: title.trim().into(),
                            url: link.trim().into(),
                            source: source.into(),
                            provider: source.into(),
                            published: published.timestamp(),
                            assets,
                            category,
                            impact,
                            reason,
                        });
                    }
                    in_item = false;
                }
                field.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(articles)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn article(title: &str, url: &str, published: i64) -> Article {
        Article {
            title: title.into(),
            url: url.into(),
            source: "test".into(),
            provider: "test".into(),
            published,
            assets: vec!["BTC".into()],
            category: "Market".into(),
            impact: 35,
            reason: String::new(),
        }
    }

    #[test]
    fn feed_reaction_requests_exist_without_opening_and_follow_filters() {
        let now = chrono::Utc::now().timestamp();
        let mut tracker = NewsTracker::new(Config::default());
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![],
            articles: vec![
                Article {
                    assets: vec!["ETH".into()],
                    ..article("Ethereum upgrade", "https://example.com/a", now - 1000)
                },
                Article {
                    assets: vec!["ETH".into()],
                    ..article("Ethereum upgrade", "https://example.com/b", now - 1000)
                },
                article("Bitcoin adoption", "https://example.com/c", now - 2000),
            ],
        }));
        assert!(tracker.selected.is_none());
        let requests = tracker.feed_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].asset, "ETH");
        tracker.update(Message::FeedReactionResult(
            requests[0].clone(),
            Err("unsupported".into()),
        ));
        assert!(tracker.feed_reactions.contains_key(&requests[0]));
        assert!(tracker.selected.is_none());
        assert!(matches!(tracker.reaction_state, reaction::State::Idle));
        let mut stale = requests[0].clone();
        stale.published -= 60;
        tracker.update(Message::FeedReactionResult(
            stale.clone(),
            Err("obsolete".into()),
        ));
        assert!(!tracker.feed_reactions.contains_key(&stale));
        tracker.update(Message::Search("Bitcoin".into()));
        assert_eq!(tracker.feed_requests().len(), 1);
        assert_eq!(tracker.feed_requests()[0].asset, "BTC");
    }

    #[test]
    fn reaction_results_cannot_overwrite_another_article_or_asset() {
        let now = chrono::Utc::now().timestamp();
        let mut tracker = NewsTracker::new(Config::default());
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![],
            articles: vec![article("BTC story", "https://example.com/a", now - 90_000)],
        }));
        tracker.update(Message::Select("https://example.com/a".into()));
        let request = reaction::Request {
            article_url: "https://example.com/a".into(),
            asset: "BTC".into(),
            published: now - 90_000,
        };
        let nonce = tracker.reaction_nonce;
        tracker.update(Message::ReactionAsset("ETH".into()));
        tracker.update(Message::ReactionResult(
            request,
            nonce,
            Err("old error".into()),
        ));
        assert!(matches!(tracker.reaction_state, reaction::State::Loading));
        let request = reaction::Request {
            article_url: "https://example.com/a".into(),
            asset: "ETH".into(),
            published: now - 90_000,
        };
        tracker.update(Message::CloseDetail);
        tracker.update(Message::ReactionResult(
            request,
            tracker.reaction_nonce,
            Err("closed article error".into()),
        ));
        assert!(matches!(tracker.reaction_state, reaction::State::Loading));
    }

    #[test]
    fn manual_refresh_pause_pending_and_selection_are_consistent() {
        let now = chrono::Utc::now().timestamp();
        let mut tracker = NewsTracker::new(Config::default());
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![],
            articles: vec![article("Bitcoin story", "https://example.com/a", now)],
        }));
        tracker.update(Message::Select("https://example.com/a".into()));
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![],
            articles: vec![article("New BTC story", "https://example.com/b", now + 1)],
        }));
        assert_eq!(tracker.articles.len(), 1);
        assert_eq!(tracker.pending.len(), 1);
        tracker.update(Message::RevealNew);
        assert_eq!(tracker.articles.len(), 2);
        assert!(tracker.selected.is_none());
        tracker.update(Message::Select("https://example.com/a".into()));
        tracker.update(Message::Search("nothing-matches".into()));
        assert!(tracker.selected.is_none());
        tracker.update(Message::Pause(true));
        tracker.update(Message::Refresh);
        let generation = tracker.generation;
        tracker.update(Message::FetchedGeneration(
            generation - 1,
            Batch {
                fetched: 1,
                sources: vec![],
                articles: vec![],
            },
        ));
        assert_ne!(tracker.fetched, Some(1));
        tracker.update(Message::FetchedGeneration(
            generation,
            Batch {
                fetched: now,
                sources: vec![],
                articles: vec![],
            },
        ));
        assert!(tracker.paused);
        assert!(!tracker.manual_refresh);
        assert!(!tracker.loading);
    }

    #[test]
    fn failed_sources_keep_success_time_and_corroborating_links() {
        let now = chrono::Utc::now().timestamp();
        let mut tracker = NewsTracker::new(Config::default());
        let source = SourceState {
            name: "CoinDesk".into(),
            status: "OK".into(),
            healthy: true,
            configured: true,
            last_success: Some(now),
        };
        let mut a = article("BTC event", "https://example.com/a", now);
        a.source = "CoinDesk".into();
        a.provider = "CoinDesk".into();
        let mut b = article("BTC event", "https://example.com/b", now);
        b.source = "Decrypt".into();
        b.provider = "Decrypt".into();
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![source.clone()],
            articles: vec![a, b],
        }));
        assert_eq!(tracker.articles.len(), 2);
        let failure = SourceState {
            healthy: false,
            status: "network error".into(),
            last_success: None,
            ..source
        };
        tracker.update(Message::Fetched(Batch {
            fetched: now + 1,
            sources: vec![failure],
            articles: vec![],
        }));
        assert_eq!(tracker.sources[0].last_success, Some(now));
        assert!(tracker.stale(&tracker.articles[0]));
    }

    #[test]
    fn news_merge_retains_failed_sources_and_bounds_history() {
        let now = chrono::Utc::now().timestamp();
        let mut tracker = NewsTracker::new(Config::default());
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![],
            articles: vec![
                article("Bitcoin story", "https://example.com/a?utm_source=x", now),
                article("Bitcoin story", "https://example.com/a", now),
                article("Expired", "https://example.com/old", now - 8 * 86400),
            ],
        }));
        assert_eq!(tracker.articles.len(), 1);
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![SourceState {
                name: "test".into(),
                status: "network error".into(),
                healthy: false,
                configured: true,
                last_success: None,
            }],
            articles: vec![],
        }));
        assert_eq!(tracker.articles.len(), 1);
        tracker.update(Message::Pause(true));
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![],
            articles: vec![article("Ignored", "https://example.com/b", now)],
        }));
        assert_eq!(tracker.articles.len(), 1);
        tracker.update(Message::Refresh);
        assert!(tracker.paused);
        assert_eq!(tracker.generation, 2);
        tracker.update(Message::Fetched(Batch {
            fetched: now,
            sources: vec![],
            articles: (0..600)
                .map(|n| {
                    article(
                        &format!("Story {n}"),
                        &format!("https://example.com/{n}"),
                        now,
                    )
                })
                .collect(),
        }));
        assert_eq!(tracker.articles.len(), 500);
    }

    #[test]
    #[ignore = "requires network access to the live RSS publishers"]
    fn live_news_sources() {
        let batch = tokio::runtime::Runtime::new().unwrap().block_on(fetch());
        for (name, _) in FEEDS {
            let count = batch.articles.iter().filter(|a| a.source == name).count();
            println!("{name}: {count} parsed headlines");
            assert!(
                count > 0,
                "{}",
                batch
                    .sources
                    .iter()
                    .find(|s| s.name == name)
                    .unwrap()
                    .status
            );
        }
        let mut tracker = NewsTracker::new(Config::default());
        tracker.update(Message::Fetched(batch));
        assert!(!tracker.articles.is_empty());
        println!("Retained: {} headlines", tracker.articles.len());
    }

    #[test]
    fn rss_entities_cdata_dates_and_safe_links() {
        let xml = br#"<rss><channel><item><title>Bitcoin &amp; ETH &#38; ETF</title><link><![CDATA[https://example.com/a]]></link><pubDate>Fri, 09 Oct 2026 10:00:00 +0000</pubDate></item><item><title>bad</title><link>file:///secret</link><pubDate>Fri, 09 Oct 2026 10:00:00 +0000</pubDate></item></channel></rss>"#;
        let articles = parse_rss(xml, "test").unwrap();
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "Bitcoin & ETH & ETF");
        assert_eq!(articles[0].assets, ["BTC", "ETH"]);
        assert!(parse_rss(b"<rss><item></rss>", "test").is_err());
    }
    #[test]
    fn cryptopanic_supports_plan_dependent_fields() {
        let value = serde_json::json!({"results": [{"title":"SOL upgrade", "original_url":"https://example.com/source", "url":"https://cryptopanic.com/news/1", "published_at":"2026-10-09T10:00:00Z", "instruments":[{"code":"SOL"}]}]});
        let articles = parse_cryptopanic(&value).unwrap();
        assert_eq!(articles[0].url, "https://example.com/source");
        assert_eq!(articles[0].assets, ["SOL"]);
        assert_eq!(articles[0].source, "CryptoPanic");
        assert!(parse_cryptopanic(&serde_json::json!({"error":"quota"})).is_err());
    }
    #[test]
    fn canonical_links_keep_meaningful_queries() {
        assert_eq!(
            canonical_url("https://example.com/a?id=1&utm_source=x#top"),
            "https://example.com/a?id=1"
        );
        assert!(!valid_url("javascript:alert(1)"));
    }
}

#[cfg(test)]
#[path = "news/preview.rs"]
mod preview;
