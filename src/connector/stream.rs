use std::time::{Duration, Instant};

use data::stream::PersistStreamKind;
use exchange::adapter::StreamKind;

/// Persisted stream resolution to avoid loop retries
const RESOLVE_RETRY_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedStream {
    /// Streams that are persisted but needs to be resolved for use
    Waiting {
        streams: Vec<PersistStreamKind>,
        last_attempt: Option<Instant>,
    },
    /// Streams that are active and ready to use, but can't persist
    Ready(Vec<StreamKind>),
    /// A tape with an active primary and secondary tickers awaiting metadata.
    PartiallyReady {
        ready: Vec<StreamKind>,
        configured: Vec<PersistStreamKind>,
        last_attempt: Option<Instant>,
    },
    Blocked {
        streams: Vec<PersistStreamKind>,
        reason: String,
        last_attempt: Option<Instant>,
    },
}

impl ResolvedStream {
    pub fn waiting(streams: Vec<PersistStreamKind>) -> Self {
        ResolvedStream::Waiting {
            streams,
            last_attempt: None,
        }
    }

    /// Returns streams to resolve only if the retry interval has elapsed.
    pub fn due_streams_to_resolve(&mut self, now: Instant) -> Option<Vec<PersistStreamKind>> {
        let (streams, last_attempt) = match self {
            ResolvedStream::Waiting {
                streams,
                last_attempt,
            } => (streams, last_attempt),
            ResolvedStream::Blocked {
                streams,
                last_attempt,
                ..
            }
            | ResolvedStream::PartiallyReady {
                configured: streams,
                last_attempt,
                ..
            } => (streams, last_attempt),
            _ => return None,
        };

        if streams.is_empty() {
            log::trace!(
                "STREAM ResolveDue | waiting_streams=0 last_attempt={:?} should_retry=false reason=empty_waiting",
                last_attempt
            );
            return None;
        }

        let should_retry = last_attempt
            .map(|t| now.duration_since(t) >= RESOLVE_RETRY_INTERVAL)
            .unwrap_or(true);

        if !should_retry {
            log::trace!(
                "STREAM ResolveDue | waiting_streams={} last_attempt={:?} should_retry=false reason=retry_interval",
                streams.len(),
                last_attempt
            );
            return None;
        }

        *last_attempt = Some(now);
        log::debug!(
            "STREAM ResolveDue | waiting_streams={} last_attempt={:?} should_retry=true returned_streams={:?}",
            streams.len(),
            last_attempt,
            streams
        );
        Some(streams.clone())
    }

    pub fn matches_stream(&self, stream: &StreamKind) -> bool {
        self.ready_iter().is_some_and(|mut existing| {
            existing.any(|candidate| {
                match (candidate.as_trade_stream(), stream.as_trade_stream()) {
                    (Some(a), Some(b)) => a.ticker == b.ticker,
                    _ => candidate == stream,
                }
            })
        })
    }

    pub fn ready_iter_mut(&mut self) -> Option<impl Iterator<Item = &mut StreamKind>> {
        match self {
            ResolvedStream::Ready(streams)
            | ResolvedStream::PartiallyReady { ready: streams, .. } => Some(streams.iter_mut()),
            _ => None,
        }
    }

    pub fn ready_iter(&self) -> Option<impl Iterator<Item = &StreamKind>> {
        match self {
            ResolvedStream::Ready(streams)
            | ResolvedStream::PartiallyReady { ready: streams, .. } => Some(streams.iter()),
            _ => None,
        }
    }

    pub fn find_ready_map<F, T>(&self, f: F) -> Option<T>
    where
        F: FnMut(&StreamKind) -> Option<T>,
    {
        self.ready_iter()?.find_map(f)
    }

    pub fn into_waiting(self) -> Vec<PersistStreamKind> {
        match self {
            ResolvedStream::Waiting { streams, .. } => streams,
            ResolvedStream::Ready(streams) => {
                streams.into_iter().map(PersistStreamKind::from).collect()
            }
            ResolvedStream::Blocked { streams, .. } => streams,
            ResolvedStream::PartiallyReady { configured, .. } => configured,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::{Ticker, TickerInfo, adapter::Exchange};

    #[test]
    fn trade_routing_matches_market_across_metadata_versions() {
        let ticker = Ticker::new("BTCUSDT", Exchange::BinanceLinear);
        let original = TickerInfo::new(ticker, 0.01, 0.001, None);
        let updated = TickerInfo::new(ticker, 0.1, 0.001, None);
        let other = TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BybitLinear),
            0.1,
            0.001,
            None,
        );
        let selected = ResolvedStream::Ready(vec![StreamKind::Trades {
            ticker_info: original,
        }]);
        assert!(selected.matches_stream(&StreamKind::Trades {
            ticker_info: updated
        }));
        assert!(!selected.matches_stream(&StreamKind::Trades { ticker_info: other }));
    }
}
