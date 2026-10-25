//! `parsers list | test | validate` — the pack authoring loop
//! (docs/surfaces.md §2.1). Project packs come from the discovered store's
//! `formats.d/` when there is one; user packs from
//! `$HOME/.config/loglense/formats.d/`.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use loglense_core::{Event, Stream};
use loglense_parsers::{
    load_pack, standard_registry, validate_pack, LineCtx, LoadedRegistry, Parser as _, Source,
    GENERIC,
};
use loglense_store::query;

use crate::cli::ParsersCmd;
use crate::context::{self, Ctx};
use crate::error::{CliError, EXIT_USAGE};
use crate::output;

pub(crate) fn parsers(ctx: &Ctx, cmd: ParsersCmd) -> Result<u8, CliError> {
    match cmd {
        ParsersCmd::List => list(ctx),
        ParsersCmd::Test {
            pack,
            parser,
            sample,
            max,
        } => test(
            ctx,
            pack.as_deref(),
            parser,
            &sample,
            usize::try_from(max).unwrap_or(usize::MAX),
        ),
        ParsersCmd::Validate { pack } => validate(ctx, &pack),
    }
}

/// Registry for this invocation: built-ins, user packs, and the project
/// packs of the discovered store (none when no store is found).
fn registry(ctx: &Ctx) -> LoadedRegistry {
    let project = ctx
        .reader_state()
        .map(|state| state.join("formats.d"))
        .unwrap_or_default();
    let user = context::home().join(".config/loglense/formats.d");
    standard_registry(&project, Some(&user))
}

fn source_name(source: Source) -> &'static str {
    match source {
        Source::Builtin => "built-in",
        Source::UserPack => "user",
        Source::ProjectPack => "project",
    }
}

#[derive(serde::Serialize)]
struct ListRow {
    name: String,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    extends: Option<String>,
    /// `loaded`, or `broken: <error>` for a pack that failed to load.
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
}

fn list(ctx: &Ctx) -> Result<u8, CliError> {
    let loaded = registry(ctx);
    let mut rows: Vec<ListRow> = loaded
        .registry
        .registrations()
        .into_iter()
        .map(|r| ListRow {
            name: r.name.to_string(),
            source: source_name(r.source),
            extends: r.extends.map(str::to_string),
            status: "loaded".to_string(),
            path: None,
        })
        .collect();
    rows.extend(loaded.broken.iter().map(|b| {
        ListRow {
            name: b
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            source: source_name(b.source),
            extends: None,
            status: format!("broken: {}", b.error),
            path: Some(b.path.clone()),
        }
    }));
    if ctx.json {
        output::json(&serde_json::json!({ "parsers": rows }));
        return Ok(0);
    }
    println!("{:<12} {:<9} {:<10} STATUS", "NAME", "SOURCE", "EXTENDS");
    for row in &rows {
        println!(
            "{:<12} {:<9} {:<10} {}",
            row.name,
            row.source,
            row.extends.as_deref().unwrap_or("-"),
            query::clip(&row.status)
        );
    }
    Ok(0)
}

fn test(
    ctx: &Ctx,
    pack: Option<&Path>,
    parser: Option<String>,
    sample: &Path,
    max: usize,
) -> Result<u8, CliError> {
    let mut loaded = registry(ctx);
    let mut chain_name = parser;
    if let Some(path) = pack {
        let pack = load_pack(path).map_err(|err| CliError::Usage(err.to_string()))?;
        let name = pack.name().to_string();
        let extends = pack.extends().map(str::to_string);
        let pack = Arc::new(pack);
        loaded.registry.register(
            &name,
            move || Box::new((*pack).clone()),
            Source::ProjectPack,
            extends,
        );
        chain_name.get_or_insert(name);
    }
    let Some(chain_name) = chain_name else {
        return Err(CliError::Usage(
            "give a pack file and/or --parser <name> to choose the chain".to_string(),
        ));
    };
    let chain = loaded
        .registry
        .build_chain(&chain_name)
        .map_err(|err| CliError::Usage(err.to_string()))?;
    let names = chain.parser_names();
    let mut state = chain.new_run();

    let file = std::fs::File::open(sample).map_err(|source| CliError::io(sample, source))?;
    let mut reader = std::io::BufReader::new(file);
    let mut shown: Vec<Event> = Vec::new();
    let mut total_events = 0usize;
    let mut lines = 0u64;
    let mut unclaimed = 0u64;
    let mut keep = |events: Vec<Event>| {
        for event in events {
            total_events += 1;
            if shown.len() < max {
                shown.push(event);
            }
        }
    };
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .map_err(|source| CliError::io(sample, source))?;
        if read == 0 {
            break;
        }
        if raw.last() == Some(&b'\n') {
            raw.pop();
        }
        lines += 1;
        let text = String::from_utf8_lossy(&raw);
        let ctx_line = LineCtx {
            stream: Stream::Out,
            line_no: lines,
        };
        let (events, claimer) = state.feed_traced(&text, &ctx_line);
        if claimer.is_none_or(|i| names.get(i) == Some(&GENERIC)) {
            unclaimed += 1;
        }
        keep(events);
    }
    keep(state.finish());

    if ctx.json {
        output::json(&serde_json::json!({
            "chain": names,
            "lines": lines,
            "unclaimed": unclaimed,
            "events": shown,
            "total_events": total_events,
            "shown": shown.len(),
        }));
        return Ok(0);
    }
    println!("chain: {}", names.join(" → "));
    for event in &shown {
        println!(
            "L{:<6} {:<7} {:<7} {}",
            event.lines.first().copied().unwrap_or(0),
            format!("{:?}", event.severity).to_lowercase(),
            format!("{:?}", event.kind).to_lowercase(),
            query::clip(&event.message)
        );
    }
    if total_events > shown.len() {
        println!(
            "({} of {total_events} events shown; --max to change)",
            shown.len()
        );
    }
    println!(
        "{lines} lines, {total_events} events, {unclaimed} lines unclaimed (fell through to {GENERIC})"
    );
    Ok(0)
}

fn validate(ctx: &Ctx, pack: &Path) -> Result<u8, CliError> {
    let errors: Vec<String> = validate_pack(pack)
        .iter()
        .map(ToString::to_string)
        .collect();
    if ctx.json {
        output::json(&serde_json::json!({
            "path": pack,
            "valid": errors.is_empty(),
            "errors": errors,
        }));
    } else if errors.is_empty() {
        println!("ok: {}", pack.display());
    } else {
        println!("{} error(s) in {}:", errors.len(), pack.display());
        for error in &errors {
            println!("  {error}");
        }
    }
    Ok(if errors.is_empty() { 0 } else { EXIT_USAGE })
}
