//! The parse pump: turns decoded lines into stamped, deduped events.
//!
//! Owns per-stream [`Lane`]s — sample buffering during autodetect (≤ 50
//! lines / ≤ 2 s, EOF releases early), the per-stream chain state, and the
//! 10 000-line block cap that force-flushes a runaway multi-line block
//! (docs/parsers.md §1.4, §3.3). Events get a run-monotonic `seq`, capture
//! `ts_ms`, a `template_id` from the LRU dedup tracker, and land in the
//! [`EventSink`] (a thin seam so write failures can be exercised without a
//! full disk).

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use loglense_core::template::TemplateGroups;
use loglense_core::{Event, Kind, Stream};
use loglense_parsers::{detect_from_sample, Chain, LineCtx, MultiChainState, ParserRegistry};
use loglense_store::StoreError;

use crate::summary::severity_key;

#[cfg(test)]
mod tests;

/// Autodetect sample cap (docs/parsers.md §3.3).
pub(crate) const SAMPLE_MAX_LINES: usize = 50;
/// Autodetect sample window from the first buffered line.
pub(crate) const SAMPLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
/// Consecutive claimed-but-silent lines before a block is force-flushed.
pub(crate) const BLOCK_CAP: u64 = 10_000;
/// Distinct template ids tracked for the summary's top-N selection
/// (mirrors the dedup tracker's LRU cap so memory stays bounded).
const HOT_CAP: usize = 10_000;

/// How the run selected its parsers: an explicit name or per-stream detect.
pub(crate) enum Selection {
    /// Explicit `--parser` / config default — no buffering delay.
    Named(String),
    /// Detect per stream from the first ≤ 50 lines / ≤ 2 s.
    Auto,
}

/// Append seam over `events.jsonl` (tests inject failing sinks).
pub(crate) trait EventSink {
    /// Appends one event; an error means the spool is unusable (ENOSPC…).
    fn append(&mut self, event: &Event) -> Result<(), StoreError>;
}

/// Everything the pump learned during the run (feeds `summary.json`).
#[derive(Debug, Default)]
pub(crate) struct Capture {
    /// Events successfully appended.
    pub events: u64,
    /// Events per severity wire-name.
    pub severities: BTreeMap<String, u64>,
    /// `kind=test` events per outcome.
    pub tests: BTreeMap<String, u64>,
    /// Template dedup tracker (LRU-capped).
    pub templates: TemplateGroups,
    /// Distinct template ids seen first (bounded, feeds top-N selection).
    pub hot_ids: Vec<String>,
    /// Set when a spool write failed: parsing stops, raw capture continues.
    pub truncated: Option<String>,
    /// Parser detected for stdout (autodetect only).
    pub parser_out: Option<String>,
    /// Parser detected for stderr (autodetect only).
    pub parser_err: Option<String>,
}

/// One stream's parse state.
struct Lane {
    chain: Option<Chain>,
    state: Option<MultiChainState>,
    sample: Option<Vec<(u64, String)>>,
    deadline: Option<Instant>,
    unclaimed: u64,
    detected: Option<String>,
}

impl Lane {
    fn auto() -> Self {
        Self {
            chain: None,
            state: None,
            sample: Some(Vec::new()),
            deadline: None,
            unclaimed: 0,
            detected: None,
        }
    }

    /// Explicit selection: chain built up front (registry validated the name
    /// at spawn; a pack-induced chain failure degrades to raw-only).
    fn named(registry: &ParserRegistry, name: &str) -> Self {
        let mut lane = Lane::auto();
        lane.sample = None;
        match registry.build_chain(name) {
            Ok(chain) => {
                lane.state = Some(chain.new_run());
                lane.chain = Some(chain);
            }
            Err(_) => lane.chain = None,
        }
        lane.detected = Some(name.to_string());
        lane
    }

    fn drain(&mut self) {
        self.chain = None;
        self.state = None;
        self.sample = None;
        self.deadline = None;
    }
}

/// Line intake + event stamping for one run, both streams.
pub(crate) struct Pump<'a> {
    sink: &'a mut dyn EventSink,
    registry: &'a ParserRegistry,
    selection: Selection,
    out: Lane,
    err: Lane,
    seq: u64,
    hot_seen: std::collections::HashSet<String>,
    hot_ids: Vec<String>,
    capture: Capture,
}

impl<'a> Pump<'a> {
    pub(crate) fn new(
        sink: &'a mut dyn EventSink,
        registry: &'a ParserRegistry,
        selection: Selection,
    ) -> Self {
        let (out, err) = match &selection {
            Selection::Named(name) => (Lane::named(registry, name), Lane::named(registry, name)),
            Selection::Auto => (Lane::auto(), Lane::auto()),
        };
        let mut pump = Self {
            sink,
            registry,
            selection,
            out,
            err,
            seq: 0,
            hot_seen: std::collections::HashSet::new(),
            hot_ids: Vec::new(),
            capture: Capture::default(),
        };
        if let Selection::Named(name) = &pump.selection {
            if pump.out.chain.is_none() {
                pump.stop_parsing(format!("cannot build parser chain `{name}`"));
            }
        }
        pump
    }

    fn lane(&mut self, stream: Stream) -> &mut Lane {
        match stream {
            Stream::Out => &mut self.out,
            Stream::Err => &mut self.err,
        }
    }

    /// One decoded (ANSI-stripped) line at raw `line_no` of `stream`.
    pub(crate) fn handle_line(&mut self, stream: Stream, line_no: u64, text: &str) {
        if self.capture.truncated.is_some() {
            return;
        }
        if self.lane(stream).sample.is_none() {
            self.feed(stream, line_no, text);
            return;
        }
        let full;
        {
            let lane = self.lane(stream);
            if lane.sample.as_ref().is_some_and(Vec::is_empty) {
                lane.deadline = Some(Instant::now() + SAMPLE_WINDOW);
            }
            let Some(sample) = lane.sample.as_mut() else {
                return;
            };
            sample.push((line_no, text.to_string()));
            full = sample.len() >= SAMPLE_MAX_LINES;
        }
        if full {
            self.resolve_detection(stream);
        }
    }

    /// Binary stream (NUL in first 4 KB): parsing disabled, raw continues.
    pub(crate) fn handle_binary(&mut self, stream: Stream) {
        self.lane(stream).drain();
    }

    /// End of stream: release any pending sample, flush open blocks.
    pub(crate) fn handle_eof(&mut self, stream: Stream) {
        if self.capture.truncated.is_some() {
            return;
        }
        if self.sample_len(stream) > 0 {
            self.resolve_detection(stream);
        } else {
            self.lane(stream).sample = None;
        }
        let flushed = self
            .lane(stream)
            .state
            .take()
            .map_or_else(Vec::new, |mut state| state.finish());
        self.write_events(flushed);
        self.lane(stream).chain = None;
    }

    /// Resolves detection for streams whose 2 s window elapsed.
    pub(crate) fn release_deadlines(&mut self, now: Instant) {
        for stream in [Stream::Out, Stream::Err] {
            let due = self
                .lane(stream)
                .deadline
                .is_some_and(|deadline| deadline <= now);
            if due && self.sample_len(stream) > 0 {
                self.resolve_detection(stream);
            }
        }
    }

    /// Earliest pending sample deadline (sizes the capture loop's recv wait).
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        [self.out.deadline, self.err.deadline]
            .into_iter()
            .flatten()
            .min()
    }

    fn sample_len(&self, stream: Stream) -> usize {
        let lane = match stream {
            Stream::Out => &self.out,
            Stream::Err => &self.err,
        };
        lane.sample.as_ref().map_or(0, Vec::len)
    }

    fn resolve_detection(&mut self, stream: Stream) {
        let sample = self.lane(stream).sample.take().unwrap_or_default();
        self.lane(stream).deadline = None;
        if sample.is_empty() {
            return;
        }
        let name = match &self.selection {
            Selection::Named(name) => name.clone(),
            Selection::Auto => {
                let refs: Vec<&str> = sample.iter().map(|(_, text)| text.as_str()).collect();
                detect_from_sample(self.registry, &refs)
            }
        };
        match self.registry.build_chain(&name) {
            Ok(chain) => {
                self.lane(stream).state = Some(chain.new_run());
                self.lane(stream).chain = Some(chain);
            }
            Err(err) => {
                self.stop_parsing(format!("cannot build parser chain `{name}`: {err}"));
                return;
            }
        }
        self.lane(stream).detected = Some(name);
        for (line_no, text) in sample {
            self.feed(stream, line_no, &text);
        }
    }

    fn feed(&mut self, stream: Stream, line_no: u64, text: &str) {
        if self.lane(stream).chain.is_none() {
            return;
        }
        let ctx = LineCtx { stream, line_no };
        let events = self
            .lane(stream)
            .state
            .as_mut()
            .map_or_else(Vec::new, |state| state.feed(text, &ctx));
        if events.is_empty() {
            let lane = self.lane(stream);
            lane.unclaimed += 1;
            if lane.unclaimed >= BLOCK_CAP {
                self.force_flush(stream);
            }
            return;
        }
        self.write_events(events);
        self.lane(stream).unclaimed = 0;
    }

    /// Block-cap overflow: flush the open block as best-effort events and
    /// start a fresh state so a runaway block can never grow unbounded.
    fn force_flush(&mut self, stream: Stream) {
        let flushed = self
            .lane(stream)
            .state
            .take()
            .map_or_else(Vec::new, |mut state| state.finish());
        if let Some(chain) = self.lane(stream).chain.as_ref() {
            self.lane(stream).state = Some(chain.new_run());
        }
        self.lane(stream).unclaimed = 0;
        self.write_events(flushed);
    }

    fn write_events(&mut self, events: Vec<Event>) {
        for mut event in events {
            self.seq += 1;
            event.seq = self.seq;
            event.ts_ms = now_ms();
            let line = event.lines.first().copied().unwrap_or_default();
            let template_id = self.capture.templates.record(&event.message, line);
            if self.hot_seen.insert(template_id.clone()) && self.hot_ids.len() < HOT_CAP {
                self.hot_ids.push(template_id.clone());
            }
            event.template_id = Some(template_id);
            *self
                .capture
                .severities
                .entry(severity_key(event.severity).to_string())
                .or_default() += 1;
            if event.kind == Kind::Test {
                if let Some(test) = &event.test {
                    *self.capture.tests.entry(test.outcome.clone()).or_default() += 1;
                }
            }
            if let Err(err) = self.sink.append(&event) {
                self.stop_parsing(format!("events write failed: {err}"));
                return;
            }
            self.capture.events += 1;
        }
    }

    /// Spool unusable: stop parsing both streams (raw capture continues).
    pub(crate) fn stop_parsing(&mut self, reason: String) {
        self.capture.truncated = Some(reason);
        self.out.drain();
        self.err.drain();
    }

    /// Consumes the pump, yielding the run's capture accounting.
    pub(crate) fn into_capture(mut self) -> Capture {
        let mut capture = std::mem::take(&mut self.capture);
        capture.hot_ids = std::mem::take(&mut self.hot_ids);
        capture.parser_out = self.out.detected;
        capture.parser_err = self.err.detected;
        capture
    }
}

/// Epoch milliseconds for event timestamps.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
