//! Per-run parser chains (docs/parsers.md §1.4, §3.2): ordered
//! first-claim-wins intake with one [`ParseState`] per chain member per
//! stream, so multi-line blocks on stdout and stderr never interleave.

use loglense_core::{Event, Stream};

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// An immutable parser chain in evaluation order: selected parser first,
/// then ancestors (most-derived to base-most), `generic` last. Built by
/// [`ParserRegistry::build_chain`](crate::ParserRegistry::build_chain).
pub struct Chain {
    parsers: Vec<Box<dyn Parser>>,
}

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.parser_names()).finish()
    }
}

impl Chain {
    pub(crate) fn new(parsers: Vec<Box<dyn Parser>>) -> Self {
        Self { parsers }
    }

    /// Member names in evaluation order.
    pub fn parser_names(&self) -> Vec<&'static str> {
        self.parsers.iter().map(|p| p.name()).collect()
    }

    /// Fresh per-run state: one [`ParseState`] per member per stream
    /// (created eagerly for both streams).
    pub fn new_run(&self) -> MultiChainState {
        MultiChainState {
            out: self.parsers.iter().map(|p| p.new_run()).collect(),
            err: self.parsers.iter().map(|p| p.new_run()).collect(),
        }
    }
}

/// Per-run, per-stream intake state over a [`Chain`]. Lines route by
/// [`LineCtx::stream`]; each stream's states are consulted in chain order
/// and the first [`FeedOutcome::Claimed`] consumes the line. With the
/// `generic` fallback last (T7), every line is claimed; a chain without a
/// final claimer silently drops unclaimed lines.
pub struct MultiChainState {
    out: Vec<Box<dyn ParseState>>,
    err: Vec<Box<dyn ParseState>>,
}

impl MultiChainState {
    fn lane(&mut self, stream: Stream) -> &mut Vec<Box<dyn ParseState>> {
        match stream {
            Stream::Out => &mut self.out,
            Stream::Err => &mut self.err,
        }
    }

    /// Offer one line to the stream's states in chain order; the first
    /// claimer's events (possibly none — a buffered block line) are
    /// returned.
    pub fn feed(&mut self, line: &str, ctx: &LineCtx) -> Vec<Event> {
        self.feed_traced(line, ctx).0
    }

    /// [`feed`](Self::feed) that also reports which chain member (index in
    /// [`Chain::parser_names`] order) claimed the line — `None` when no
    /// member did. Pack authoring uses it to count lines that fell through
    /// to the `generic` fallback.
    pub fn feed_traced(&mut self, line: &str, ctx: &LineCtx) -> (Vec<Event>, Option<usize>) {
        for (index, state) in self.lane(ctx.stream).iter_mut().enumerate() {
            match state.feed(line, ctx) {
                FeedOutcome::Claimed(events) => return (events, Some(index)),
                FeedOutcome::Passed => continue,
            }
        }
        (Vec::new(), None)
    }

    /// End-of-stream flush: every state that may hold an open partial block
    /// emits its best-effort event. Stdout states flush before stderr ones.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for lane in [&mut self.out, &mut self.err] {
            for state in lane.iter_mut() {
                events.extend(state.finish());
            }
        }
        events
    }
}
