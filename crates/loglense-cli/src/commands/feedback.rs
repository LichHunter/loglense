//! `feedback leave | list | show | promote | resolve` — agent reports and
//! the feedback → fixture bridge (docs/surfaces.md §2.2, D-010).

use std::path::{Path, PathBuf};

use loglense_core::Stream;
use loglense_store::{
    list_feedback, query, read_feedback, read_feedback_index, read_meta, resolve_feedback,
    write_feedback, FeedbackRecord, FeedbackStatus,
};

use crate::cli::{
    FeedbackCmd, FeedbackKindArg, FeedbackStatusArg, PromoteStream, ResolveStatus, SeverityArg,
};
use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

/// Raw lines copied on each side of the reported line.
const CONTEXT_LINES: u64 = 20;
/// Fixture root inside a loglense checkout.
const FIXTURES: &str = "crates/loglense-parsers/tests/fixtures";

pub(crate) fn feedback(ctx: &Ctx, cmd: FeedbackCmd) -> Result<u8, CliError> {
    match cmd {
        FeedbackCmd::Leave {
            run_id,
            kind,
            detail,
            line,
            expected,
            snippet,
        } => leave(ctx, &run_id, kind, detail, line, expected, snippet),
        FeedbackCmd::List {
            status,
            global,
            max,
        } => list(
            ctx,
            status.map(status_of),
            global,
            usize::try_from(max).unwrap_or(usize::MAX),
        ),
        FeedbackCmd::Show { fid } => show(ctx, &fid),
        FeedbackCmd::Promote {
            fid,
            repo,
            stream,
            parser,
            force,
        } => promote(ctx, &fid, repo, stream, parser, force),
        FeedbackCmd::Resolve { fid, status, note } => resolve(ctx, &fid, status, note),
    }
}

fn leave(
    ctx: &Ctx,
    run_id: &str,
    kind: FeedbackKindArg,
    detail: String,
    line: Option<u64>,
    expected: Option<SeverityArg>,
    snippet: Option<String>,
) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let run_dir = context::run_dir(&state, run_id)?;
    let meta = read_meta(&run_dir)?;
    let record = FeedbackRecord {
        kind: kind_name(kind).to_string(),
        detail: query::clip(&detail),
        run_id: run_id.to_string(),
        parser: meta.parser,
        tool_version: meta.tool_version,
        line,
        expected: expected.map(|s| severity_name(s).to_string()),
        snippet: snippet.map(|s| query::clip(&s)),
        status: FeedbackStatus::Open,
        note: None,
    };
    let fid = write_feedback(&state, &context::home(), context::now_ms(), record)?;
    if ctx.json {
        output::json(&serde_json::json!({ "feedback_id": fid }));
    } else {
        println!("{fid}");
    }
    Ok(0)
}

#[derive(serde::Serialize)]
struct Row {
    feedback_id: String,
    status: FeedbackStatus,
    kind: String,
    run_id: String,
    /// Global listing: the record file is gone (dangling index pointer).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    orphan: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
}

fn list(
    ctx: &Ctx,
    status: Option<FeedbackStatus>,
    global: bool,
    max: usize,
) -> Result<u8, CliError> {
    let rows: Vec<Row> = if global {
        read_feedback_index(&context::home())?
            .into_iter()
            .filter(|p| status.is_none_or(|s| s == p.status))
            .map(|p| Row {
                orphan: !p.path.is_file(),
                feedback_id: p.fid,
                status: p.status,
                kind: p.kind,
                run_id: p.run_id,
                detail: None,
                path: Some(p.path),
            })
            .collect()
    } else {
        let state = ctx.reader_state()?;
        list_feedback(&state, status)
            .into_iter()
            .map(|(fid, record)| Row {
                feedback_id: fid,
                status: record.status,
                kind: record.kind,
                run_id: record.run_id,
                orphan: false,
                detail: Some(record.detail),
                path: None,
            })
            .collect()
    };
    let total = rows.len();
    let rows: Vec<Row> = rows.into_iter().take(max).collect();
    if ctx.json {
        output::json(&serde_json::json!({
            "feedback": rows,
            "total": total,
            "shown": rows.len(),
        }));
        return Ok(0);
    }
    if rows.is_empty() {
        println!("no feedback records");
        return Ok(0);
    }
    for row in &rows {
        let status = serde_json::to_value(row.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let tail = match (&row.detail, row.orphan, &row.path) {
            (_, true, Some(path)) => format!("ORPHAN (record missing: {})", path.display()),
            (Some(detail), _, _) => query::clip(detail),
            (None, _, Some(path)) => path.display().to_string(),
            _ => String::new(),
        };
        println!(
            "{:<20} {:<8} {:<15} {:<16} {}",
            row.feedback_id, status, row.kind, row.run_id, tail
        );
    }
    if total > rows.len() {
        println!("({} of {total} records shown; --max to change)", rows.len());
    }
    Ok(0)
}

fn show(ctx: &Ctx, fid: &str) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let record = read_feedback(&state, fid)?;
    if ctx.json {
        output::json(&serde_json::json!({ "feedback_id": fid, "record": record }));
        return Ok(0);
    }
    println!("{fid}  ({:?}, {})", record.status, record.kind);
    println!("run:      {}", record.run_id);
    if let Some(parser) = &record.parser {
        println!("parser:   {parser}");
    }
    if let Some(line) = record.line {
        println!("line:     {line}");
    }
    if let Some(expected) = &record.expected {
        println!("expected: {expected}");
    }
    println!("detail:   {}", record.detail);
    if let Some(snippet) = &record.snippet {
        println!("snippet:  {snippet}");
    }
    if let Some(note) = &record.note {
        println!("note:     {note}");
    }
    Ok(0)
}

fn promote(
    ctx: &Ctx,
    fid: &str,
    repo: Option<PathBuf>,
    stream: PromoteStream,
    parser: Option<String>,
    force: bool,
) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let record = read_feedback(&state, fid)?;
    let run_dir = context::run_dir(&state, &record.run_id).map_err(|_| {
        CliError::Usage(format!(
            "run {} referenced by {fid} no longer exists (removed by gc?)",
            record.run_id
        ))
    })?;
    let Some(parser) = parser.or(record.parser.clone()) else {
        return Err(CliError::Usage(format!(
            "{fid} names no parser; pass --parser <fixture-dir>"
        )));
    };
    if parser.contains(['/', '\\']) || parser.starts_with('.') {
        return Err(CliError::Usage(format!("invalid parser name `{parser}`")));
    }
    let repo = match repo.or_else(|| std::env::var_os("LOGLENSE_REPO").map(PathBuf::from)) {
        Some(repo) => repo,
        None => context::current_dir()?,
    };
    let fixtures = repo.join(FIXTURES);
    if !fixtures.is_dir() {
        return Err(CliError::Usage(format!(
            "{} is not a loglense checkout (no {FIXTURES}); pass --repo or set LOGLENSE_REPO",
            repo.display()
        )));
    }

    let line = record.line.unwrap_or(1);
    let stream = match stream {
        PromoteStream::Out => Stream::Out,
        PromoteStream::Err => Stream::Err,
        PromoteStream::Auto => auto_stream(&run_dir, line)?,
    };
    let spool = run_dir.join(match stream {
        Stream::Out => "stdout.raw",
        Stream::Err => "stderr.raw",
    });
    if !spool.is_file() {
        return Err(CliError::Usage(format!(
            "cannot promote {fid}: raw spool {} is missing",
            spool.display()
        )));
    }
    let first = line.saturating_sub(CONTEXT_LINES).max(1);
    let wanted = usize::try_from(line + CONTEXT_LINES - first + 1).unwrap_or(usize::MAX);
    let page = query::log_from(&run_dir, stream, first, wanted)?;
    let Some(last) = page.lines.last().map(|l| l.line) else {
        return Err(CliError::Usage(format!(
            "cannot promote {fid}: {} has no line {first}",
            spool.display()
        )));
    };

    let dir = fixtures.join(&parser);
    std::fs::create_dir_all(&dir).map_err(|source| CliError::io(&dir, source))?;
    let log_path = dir.join(format!("{fid}.log"));
    let expected_path = dir.join(format!("{fid}.expected.json"));
    if !force {
        for path in [&log_path, &expected_path] {
            if path.exists() {
                return Err(CliError::Usage(format!(
                    "{} already exists; pass --force to overwrite",
                    path.display()
                )));
            }
        }
    }
    let mut body = String::new();
    for l in &page.lines {
        body.push_str(&l.text);
        body.push('\n');
    }
    write(&log_path, &body)?;
    write(&expected_path, "[]\n")?;
    let command = "cargo test -p loglense-parsers --test golden";
    let stream_name = query::stream_name(stream);
    if ctx.json {
        output::json(&serde_json::json!({
            "feedback_id": fid,
            "log": log_path,
            "expected": expected_path,
            "stream": stream_name,
            "first_line": first,
            "last_line": last,
            "command": command,
        }));
        return Ok(0);
    }
    println!(
        "wrote {}  (lines {first}-{last} of {stream_name})",
        log_path.display()
    );
    println!(
        "wrote {}  (empty: the golden test fails until you fill in the expected events)",
        expected_path.display()
    );
    println!("next:  fill in the expected events, then run `{command}`");
    Ok(0)
}

/// stdout when it reaches `line`, else stderr when that does, else stdout.
fn auto_stream(run_dir: &Path, line: u64) -> Result<Stream, CliError> {
    for stream in [Stream::Out, Stream::Err] {
        if !query::log_from(run_dir, stream, line, 1)?.lines.is_empty() {
            return Ok(stream);
        }
    }
    Ok(Stream::Out)
}

fn resolve(
    ctx: &Ctx,
    fid: &str,
    status: ResolveStatus,
    note: Option<String>,
) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let status = match status {
        ResolveStatus::Fixed => FeedbackStatus::Fixed,
        ResolveStatus::Wontfix => FeedbackStatus::Wontfix,
    };
    resolve_feedback(&state, &context::home(), fid, status, note)?;
    show(ctx, fid)
}

fn write(path: &Path, text: &str) -> Result<(), CliError> {
    std::fs::write(path, text).map_err(|source| CliError::io(path, source))
}

fn status_of(arg: FeedbackStatusArg) -> FeedbackStatus {
    match arg {
        FeedbackStatusArg::Open => FeedbackStatus::Open,
        FeedbackStatusArg::Fixed => FeedbackStatus::Fixed,
        FeedbackStatusArg::Wontfix => FeedbackStatus::Wontfix,
    }
}

fn kind_name(kind: FeedbackKindArg) -> &'static str {
    match kind {
        FeedbackKindArg::ParseMiss => "parse-miss",
        FeedbackKindArg::FalsePositive => "false-positive",
        FeedbackKindArg::Bug => "bug",
        FeedbackKindArg::Feature => "feature",
        FeedbackKindArg::Other => "other",
    }
}

fn severity_name(severity: SeverityArg) -> &'static str {
    match severity {
        SeverityArg::Error => "error",
        SeverityArg::Warning => "warning",
        SeverityArg::Notice => "notice",
        SeverityArg::Info => "info",
        SeverityArg::Debug => "debug",
    }
}
