//! The `tools/call` dispatch: alias mapping, the pre-tool refresh, the six
//! engine calls, and the per-tool text and error rules
//! (the `mcp-server.md` note sections 4 and 5).

use std::path::Path;

use serde_json::Value;

use sieve_core::askindex::{ask_index_path, read_ask_index};
use sieve_core::product;
use sieve_core::wiring::{Graph, Meta, Node};
use sieve_core::workspace;
use sieve_parse::refresh::{
    ensure_fresh_children, ensure_fresh_graph, env_truthy, refresh_note, RefreshOptions,
};
use sieve_parse::{
    check_context, check_graph, check_graph_lookup, federated_check_text, format_check_report,
    format_graph_check_report, GraphCheck, LookupCheck,
};
use sieve_query::ask::{ask, format_ask, AskOptions};
use sieve_query::callers::{
    callers_saved_paths, edge_walk, render_block, resolve_symbol, CallersError, Depth, Direction,
    Hit,
};
use sieve_query::grep::{
    format_grep_result, grep_graph, zero_hit_note, GrepError, GrepOptions, DEFAULT_MAX_HITS,
};
use sieve_query::map::{build_repo_map, format_repo_map, map_saved_paths, MapOptions, RepoMap};
use sieve_query::skeleton::{format_skeleton, skeleton, skeleton_saved_paths};
use sieve_query::workspace::{
    coverage_note, federate_ask, federate_callers, federate_grep, federate_map, load_children,
    FederateAskOptions,
};
use sieve_savings::{
    append_line, ask_pack_region, savings_for, sieve_header, to_tokens, utf16_len, with_savings,
    with_savings_nl, Savings,
};

use crate::names::canonical_tool_name;

/// The function that answers `sieve_why`: `(symbol, all, root,
/// context_dir)` in, `(text, is_error)` out. The CLI crate sets it, because
/// this crate cannot depend on the CLI crate.
pub type WhyHandler = fn(&str, bool, &Path, &Path) -> (String, bool);

static WHY_HANDLER: std::sync::Mutex<Option<WhyHandler>> = std::sync::Mutex::new(None);

/// Sets the `sieve_why` handler. The last call wins.
pub fn set_why_handler(handler: WhyHandler) {
    if let Ok(mut slot) = WHY_HANDLER.lock() {
        *slot = Some(handler);
    }
}

/// `sieve_why`: runs the handler. It never runs `gh`.
fn why_tool(args: &Value, root: &Path, context_dir: &Path) -> (String, bool) {
    let symbol = symbol_arg(args);
    if symbol.is_empty() {
        return (format!("{} requires a symbol", product().tool("why")), true);
    }
    let all = args.get("all") == Some(&Value::Bool(true));
    let handler = WHY_HANDLER.lock().ok().and_then(|slot| *slot);
    let name = product().tool("why");
    let Some(handler) = handler else {
        return (format!("{name} is not available"), true);
    };
    // A panic in the handler must not stop the server.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler(&symbol, all, root, context_dir)
    }))
    .unwrap_or_else(|_| (format!("{name} failed: internal error"), true))
}

/// The text a missing graph reports for every tool that has no graceful
/// empty-result path of its own (`mcp-server.md` section 4).
fn no_graph_text() -> String {
    format!("no graph found — run `{} build` first", product().name)
}

/// Strips the active product's tool prefix from a canonical tool name, so
/// `dispatch` can match on the base suffix (`find_code`, `file_api`, ...)
/// no matter which product is active.
fn base_tool(canonical: &str) -> &str {
    let prefix = format!("{}_", product().name);
    canonical.strip_prefix(&prefix).unwrap_or(canonical)
}

/// Runs one `tools/call`: alias mapping, the refresh (skipped only for
/// `check_freshness`; per child at a workspace parent), the dispatch, then
/// the refresh-note prefix. Returns the result text and whether it is an
/// error.
pub fn call(name: &str, args: &Value, root: &Path, context_dir: &Path) -> (String, bool) {
    let canonical = canonical_tool_name(name);
    // `check_freshness` tries the lookup first, so it checks the cap itself.
    if base_tool(&canonical) != "check_freshness" {
        if let Some(text) = capped_text(context_dir) {
            return (text, true);
        }
    }
    let ws = workspace::read(context_dir);
    // A capped child stops the whole call, before any refresh or load.
    for child in ws.iter().flat_map(|w| &w.children) {
        let child_root = root.join(child);
        let child_ctx = child_root.join(product().context_dir_name());
        if let Some(text) = capped_text(&child_ctx) {
            return (format!("{child}: {text}"), true);
        }
    }
    let note = if base_tool(&canonical) == "check_freshness" {
        None
    } else {
        let opts = RefreshOptions {
            disabled: env_truthy(&product().env_var("NO_REFRESH")),
            force_hash: std::env::var(product().env_var("REFRESH")).as_deref() == Ok("hash"),
        };
        let outcome = match &ws {
            Some(ws) => ensure_fresh_children(root, &ws.children, &opts),
            None => ensure_fresh_graph(root, context_dir, &opts),
        };
        refresh_note(&outcome)
    };

    // A federated `find_code` that throws drops the refresh note, because
    // the tool call path catches the throw before it adds the note.
    let mut thrown = false;
    let federated = ws.and_then(|_| {
        if base_tool(&canonical) == "find_code" {
            let (text, is_error, did_throw) = workspace_find_code(args, root, context_dir);
            thrown = did_throw;
            return Some((text, is_error));
        }
        call_workspace(base_tool(&canonical), args, root, context_dir)
    });
    let (text, is_error) =
        federated.unwrap_or_else(|| dispatch(&canonical, name, args, root, context_dir));

    let text = match note {
        Some(n) if !thrown => format!("{n}\n{text}"),
        _ => text,
    };
    (text, is_error)
}

/// The workspace-parent tools (P4-44, `callWorkspaceTool`): `trace_calls`,
/// `find_all`, `repo_map` and `check_freshness` federate across the children.
/// `None` sends the call to the single-graph path. `find_code` federates `ask`
/// (workspace step 5, P1-56).
fn call_workspace(
    tool: &str,
    args: &Value,
    root: &Path,
    context_dir: &Path,
) -> Option<(String, bool)> {
    let name = product().name;
    if !matches!(
        tool,
        "trace_calls" | "find_all" | "repo_map" | "check_freshness"
    ) {
        return None;
    }
    let graphs = load_children(root, context_dir);
    match tool {
        "trace_calls" => {
            let query = symbol_arg(args);
            if query.is_empty() {
                return Some((
                    format!("{} requires a symbol", product().tool("trace_calls")),
                    true,
                ));
            }
            let direction = if str_arg(args, "direction") == Some("out") {
                Direction::Out
            } else {
                Direction::In
            };
            // A finite number of 1 or more floors; the strings `all` and
            // `full` are not read here.
            let depth = match args.get("depth").and_then(Value::as_f64) {
                Some(f) if f.is_finite() && f >= 1.0 => Depth(Some(f.floor() as usize)),
                _ => Depth(Some(1)),
            };
            let fed = federate_callers(
                &graphs,
                &query,
                str_arg(args, "in"),
                direction,
                depth,
                |graph, body, paths| {
                    with_savings(name, body, savings_for(graph, paths).as_ref())
                        .trim_end_matches('\n')
                        .to_string()
                },
            );
            Some(match fed {
                Ok(f) => (f.text, !f.found),
                Err(e) => (e.to_string(), true),
            })
        }
        "find_all" => {
            let pattern = js_string_arg(args, "pattern");
            if pattern.is_empty() {
                return Some((
                    format!("{} requires a pattern", product().tool("find_all")),
                    true,
                ));
            }
            // `in` is not read here: a parent drops it.
            let opts = GrepOptions {
                ignore_case: args.get("ignore_case") == Some(&Value::Bool(true)),
                fixed: args.get("fixed") == Some(&Value::Bool(true)),
                in_prefix: None,
                max_hits: DEFAULT_MAX_HITS,
            };
            let fed = match federate_grep(&graphs, &pattern, &opts, |g, paths| {
                savings_for(g, paths).map(|s| (s.files, s.baseline_chars))
            }) {
                Ok(f) => f,
                Err(GrepError::InvalidPattern { message, .. }) => return Some((message, true)),
                Err(e) => return Some((e.to_string(), true)),
            };
            // The grep result adds the summed savings header, kept
            // only when the baseline is above zero.
            let saved = (fed.saved_chars > 0).then_some(Savings {
                baseline_chars: fed.saved_chars,
                files: fed.saved_files,
            });
            let text = if fed.result.total_hits == 0 {
                zero_hit_note(&fed.result)
            } else {
                let body = format_grep_result(&fed.result);
                with_savings(name, &body, saved.as_ref())
            };
            let coverage = coverage_note(&graphs);
            let text = if coverage.is_empty() {
                text
            } else {
                format!("{text}\n{coverage}")
            };
            Some((text, false))
        }
        "repo_map" => {
            let max_dirs = args
                .get("max_dirs")
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite() && *n > 0.0)
                .map(|n| n as usize);
            let text = federate_map(&graphs, max_dirs, |graph, body| {
                let saved = savings_for(graph, &map_saved_paths(graph));
                with_savings(name, body, saved.as_ref())
                    .trim_end_matches('\n')
                    .to_string()
            });
            Some((text, false))
        }
        _ => {
            let loaded: Vec<(&str, &Path)> = graphs
                .loaded
                .iter()
                .map(|c| (c.child.as_str(), c.root.as_path()))
                .collect();
            let coverage = coverage_note(&graphs);
            Some(
                match federated_check_text(&loaded, &graphs.missing, &coverage) {
                    Ok((text, _)) => (text, false),
                    Err(e) => (e.to_string(), true),
                },
            )
        }
    }
}

/// `String(args.symbol ?? args.file ?? '')`.
fn symbol_arg(args: &Value) -> String {
    match args.get("symbol") {
        None | Some(Value::Null) => js_string_arg(args, "file"),
        Some(v) => js_string(v),
    }
}

fn dispatch(
    canonical: &str,
    raw_name: &str,
    args: &Value,
    root: &Path,
    context_dir: &Path,
) -> (String, bool) {
    match base_tool(canonical) {
        "find_code" => find_code(args, root, context_dir),
        "file_api" => file_api(args, context_dir),
        "check_freshness" => check_freshness(root, context_dir),
        "trace_calls" => trace_calls(args, context_dir),
        "find_all" => find_all(args, root, context_dir),
        "repo_map" => repo_map(args, context_dir),
        "why" => why_tool(args, root, context_dir),
        _ => (format!("unknown tool: {raw_name}"), true),
    }
}

/// The default `wiring.json` size cap in bytes: 64 MB.
const DEFAULT_WIRING_CAP: u64 = 64 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// A test seam for the cap; the process env stays untouched.
    static TEST_WIRING_CAP: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Sets the cap for the current test thread only.
#[cfg(test)]
fn set_test_wiring_cap(cap: Option<u64>) {
    TEST_WIRING_CAP.with(|c| c.set(cap));
}

/// Parses a cap value; a bad or missing value gives the default.
fn parse_wiring_cap(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_WIRING_CAP)
}

// ponytail: copy of the sieve-cli helper in hook.rs; move both to
// sieve-core when a third caller appears.
/// The `wiring.json` size cap: env `<PRODUCT>_WIRING_CAP_BYTES`, else 64 MB.
fn wiring_cap() -> u64 {
    #[cfg(test)]
    if let Some(cap) = TEST_WIRING_CAP.with(|c| c.get()) {
        return cap;
    }
    parse_wiring_cap(
        std::env::var(product().env_var("WIRING_CAP_BYTES"))
            .ok()
            .as_deref(),
    )
}

/// The capped-index tool error text when `wiring.json` is over the cap.
/// Reads only the file size, never the file.
fn capped_text(context_dir: &Path) -> Option<String> {
    let size = std::fs::metadata(context_dir.join(".graph").join("wiring.json"))
        .ok()?
        .len();
    let cap = wiring_cap();
    (size > cap).then(|| {
        format!(
            "the index is over the size cap: wiring.json is {size} bytes \
             and the cap is {cap} bytes; set {} to raise the cap",
            product().env_var("WIRING_CAP_BYTES")
        )
    })
}

/// Loads `wiring.json`, or `None` when it is missing or unreadable.
fn load_graph(context_dir: &Path) -> Option<Graph> {
    let path = context_dir.join(".graph").join("wiring.json");
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// An empty graph, for `sieve_find_code`'s no-graph path: `ask` runs over
/// an empty node set instead of erroring (`ask-ranking.md` section 0).
fn empty_graph() -> Graph {
    Graph {
        meta: Meta {
            version: 1,
            node_count: 0,
            edge_count: 0,
            languages: Vec::new(),
            scopes: Vec::new(),
        },
        nodes: Vec::new(),
        edges: Vec::new(),
    }
}

/// Keeps only the first occurrence of each path, in encounter order.
fn dedup_paths<I: IntoIterator<Item = String>>(paths: I) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
    out
}

/// A string argument that is read with `typeof x === 'string' && x`
/// (`in`, `direction`): a non-empty string, or `None`.
fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// A string argument that is read with `String(x ?? '')`:
/// a missing or null value is `""`, every other JSON
/// value is its JavaScript string form. The caller's `!x` test is then an
/// empty-string test.
fn js_string_arg(args: &Value, key: &str) -> String {
    match args.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(v) => js_string(v),
    }
}

/// JavaScript's `String(value)` for a JSON value.
pub(crate) fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => js_number_text(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|v| match v {
                Value::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// JavaScript's `String(number)`: an integer prints with no fraction,
/// every other finite number in its shortest round-trip form.
pub(crate) fn js_number_text(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

/// The rendered body with the escalation nudge cut away and the header
/// kept, for the `ask --source` pack measurement (section 7.9).
/// The same cut `sieve-cli/src/ask.rs` makes.
fn pack_region(rendered: &str) -> String {
    ask_pack_region(product().name, rendered)
}

/// The `sieve_find_code` savings sentence, or `None` when the rule omits
/// it: no baseline, a zero baseline, or a pack no smaller than the
/// baseline (section 7.9).
fn find_code_savings_line(graph: &Graph, paths: &[String], rendered_body: &str) -> Option<String> {
    let saved = savings_for(graph, paths)?;
    if saved.baseline_chars == 0 {
        return None;
    }
    let pack = to_tokens(utf16_len(&pack_region(rendered_body)));
    let base = to_tokens(saved.baseline_chars);
    if base <= pack {
        return None;
    }
    let delta = base - pack;
    Some(sieve_header(product().name, delta))
}

/// `find_code` at a workspace parent (P4-44): the
/// federated `ask`, with source inlined and no savings line. A thrown
/// error, such as an unknown `in` child, is the message as an error.
/// The third value is true when the federation threw.
fn workspace_find_code(args: &Value, root: &Path, context_dir: &Path) -> (String, bool, bool) {
    let query = js_string_arg(args, "query");
    if query.is_empty() {
        return (
            format!("{} requires a query", product().tool("find_code")),
            true,
            false,
        );
    }
    let opts = FederateAskOptions {
        limit: limit_arg(args),
        source: true,
        full: args.get("full") == Some(&Value::Bool(true)),
        in_prefix: str_arg(args, "in").map(str::to_string),
    };
    let graphs = load_children(root, context_dir);
    match federate_ask(&graphs, &query, &opts) {
        Ok(result) => (format_ask(&result), false, false),
        Err(e) => (e.to_string(), true, true),
    }
}

/// `typeof args.limit === 'number' ? args.limit: 5`: the number reaches
/// `slice(0, limit)` as is, so `0` and a negative limit both select no hit, and
/// a fraction truncates. ponytail: a negative limit on a structural answer
/// drops hits from the end; Sieve maps it to zero hits, the lexical outcome.
fn limit_arg(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_f64)
        .map(|n| if n > 0.0 { n as usize } else { 0 })
        .unwrap_or(5)
}

fn find_code(args: &Value, root: &Path, context_dir: &Path) -> (String, bool) {
    let query = js_string_arg(args, "query");
    if query.is_empty() {
        return (
            format!("{} requires a query", product().tool("find_code")),
            true,
        );
    }

    let graph = load_graph(context_dir).unwrap_or_else(empty_graph);
    let index = read_ask_index(&ask_index_path(context_dir));

    let limit = limit_arg(args);
    let full = args.get("full") == Some(&Value::Bool(true));
    let in_prefix = str_arg(args, "in").map(str::to_string);

    let opts = AskOptions {
        limit,
        in_prefix,
        graph_rank: true,
        source: true,
        full,
    };

    let result = match ask(&graph, index.as_ref(), &query, &opts, root) {
        Ok(r) => r,
        Err(e) => return (e.to_string(), true),
    };

    let body = format_ask(&result);
    let paths = dedup_paths(result.hits.iter().map(|h| h.path.clone()));
    let text = match find_code_savings_line(&graph, &paths, &body) {
        Some(line) => append_line(&body, &line),
        None => body,
    };
    (text, false)
}

fn file_api(args: &Value, context_dir: &Path) -> (String, bool) {
    let file = js_string_arg(args, "file");
    if file.is_empty() {
        return (
            format!("{} requires a file", product().tool("file_api")),
            true,
        );
    }

    let graph = load_graph(context_dir);
    let r = skeleton(graph.as_ref(), &file);
    // `formatSkeleton` measures the savings pack on the body with no
    // trailing newline, then adds exactly one newline after the whole
    // header-plus-body string (`mcp-server.md` section 4,;
    // mirrors `sieve-cli/src/skeleton.rs`'s own fix for the same rounding
    // boundary).
    let rendered = format_skeleton(&r);
    let body = rendered.strip_suffix('\n').unwrap_or(&rendered);
    let paths = skeleton_saved_paths(&r);
    let saved = graph.as_ref().and_then(|g| savings_for(g, &paths));
    let text = with_savings_nl(product().name, body, saved.as_ref());
    let is_error = r.entries.is_empty() && r.note.is_some();
    (text, is_error)
}

fn check_freshness(root: &Path, context_dir: &Path) -> (String, bool) {
    let g = match check_graph_lookup(root, context_dir) {
        Ok(LookupCheck::Count(g)) => g,
        Ok(LookupCheck::OverCap(n)) => {
            let name = product().name;
            let noun = if n == 1 { "file" } else { "files" };
            return (
                format!("{name} is behind on ~{n} {noun} \u{2014} run {name} build"),
                false,
            );
        }
        // No valid lookup: run the old path.
        Ok(LookupCheck::NoLookup) | Err(_) => match full_graph_check(root, context_dir) {
            Ok(g) => g,
            Err(done) => return done,
        },
    };
    let c = check_context(root, context_dir);

    // The deep-layer part shows only when a manifest exists.
    let text = if c.missing {
        format_graph_check_report(&g)
    } else if g.missing {
        format_check_report(&c)
    } else {
        format!(
            "{}\n\n{}",
            format_check_report(&c),
            format_graph_check_report(&g)
        )
    };
    (text, false)
}

/// Today's path: the cap check, then the full `check_graph`. The error
/// value is the finished tool result.
fn full_graph_check(root: &Path, context_dir: &Path) -> Result<GraphCheck, (String, bool)> {
    if let Some(text) = capped_text(context_dir) {
        return Err((text, true));
    }
    check_graph(root, context_dir).map_err(|e| (e.to_string(), true))
}

/// Renders the `sieve_trace_calls` text: one tree per matched symbol,
/// joined by a blank line, with no quoted call-site lines.
fn render_matches(matches: &[(&Node, Vec<Hit>)], direction: Direction) -> String {
    let candidate_count = matches.len();
    let blocks: Vec<String> = matches
        .iter()
        .map(|(symbol, hits)| render_block(symbol, hits, direction, candidate_count, None))
        .collect();
    blocks.join("\n\n")
}

/// `depth` as reads it: the exact strings `all` and
/// `full` mean the full closure, a finite number of 1 or more floors, and
/// everything else (a numeric string, `ALL`, a fraction under 1) is 1.
fn parse_depth_arg(args: &Value) -> Depth {
    match args.get("depth") {
        Some(Value::String(s)) if s == "all" || s == "full" => Depth(None),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f.is_finite() && f >= 1.0 => Depth(Some(f.floor() as usize)),
            _ => Depth(Some(1)),
        },
        _ => Depth(Some(1)),
    }
}

/// The text `sieve_trace_calls` prints for a symbol the graph does not
/// hold (`unknownSymbolText`): the CLI's wording, with
/// the build command in backticks.
fn unknown_symbol_text(query: &str) -> String {
    format!(
        "no symbol named {query} — try {} grep {query}",
        product().name
    )
}

fn trace_calls(args: &Value, context_dir: &Path) -> (String, bool) {
    let query = symbol_arg(args);
    if query.is_empty() {
        return (
            format!("{} requires a symbol", product().tool("trace_calls")),
            true,
        );
    }

    let Some(graph) = load_graph(context_dir) else {
        return (no_graph_text(), true);
    };

    let in_prefix = str_arg(args, "in");
    let matches = match resolve_symbol(&graph, &query, in_prefix) {
        Ok(m) => m,
        Err(CallersError::NoSymbol { .. }) => return (unknown_symbol_text(&query), true),
        Err(e) => return (e.to_string(), true),
    };

    let direction = if str_arg(args, "direction") == Some("out") {
        Direction::Out
    } else {
        Direction::In
    };
    let depth = parse_depth_arg(args);

    let pairs: Vec<(&Node, Vec<Hit>)> = matches
        .iter()
        .map(|&symbol| (symbol, edge_walk(&graph, symbol, direction, depth)))
        .collect();

    let body = render_matches(&pairs, direction);
    let paths = callers_saved_paths(&pairs);
    let saved = savings_for(&graph, &paths);
    let text = with_savings(product().name, &body, saved.as_ref());
    (text, false)
}

fn find_all(args: &Value, root: &Path, context_dir: &Path) -> (String, bool) {
    let pattern = js_string_arg(args, "pattern");
    if pattern.is_empty() {
        return (
            format!("{} requires a pattern", product().tool("find_all")),
            true,
        );
    }

    let Some(graph) = load_graph(context_dir) else {
        return (no_graph_text(), true);
    };

    let opts = GrepOptions {
        ignore_case: args.get("ignore_case") == Some(&Value::Bool(true)),
        fixed: args.get("fixed") == Some(&Value::Bool(true)),
        in_prefix: str_arg(args, "in").map(str::to_string),
        max_hits: DEFAULT_MAX_HITS,
    };

    let result = match grep_graph(&graph, root, &pattern, &opts) {
        Ok(r) => r,
        // The thrown `RegExp` message prints as is; the `invalid pattern`
        // prefix is the CLI's.
        Err(GrepError::InvalidPattern { message, .. }) => return (message, true),
        Err(e) => return (e.to_string(), true),
    };

    if result.total_hits == 0 {
        return (zero_hit_note(&result), false);
    }
    // The savings header is added here.
    let paths = dedup_paths(result.groups.iter().map(|g| g.path.clone()));
    let saved = savings_for(&graph, &paths);
    let body = format_grep_result(&result);
    (with_savings(product().name, &body, saved.as_ref()), false)
}

fn repo_map(args: &Value, context_dir: &Path) -> (String, bool) {
    let Some(graph) = load_graph(context_dir) else {
        return (no_graph_text(), true);
    };

    // `typeof max_dirs === 'number' && isFinite && > 0`.
    let max_dirs = args
        .get("max_dirs")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0);
    let mut opts = MapOptions::default();
    if let Some(n) = max_dirs {
        opts.max_dirs = n as usize;
    }

    let map = build_repo_map(&graph, &opts);
    // Strip-then-restore the trailing newline before measuring the
    // savings pack; same rounding-boundary fix as `file_api`
    // (`mcp-server.md` section 4).
    let mut rendered = format_repo_map(&map);
    if let Some(n) = max_dirs.filter(|n| n.fract() != 0.0) {
        rendered = fractional_dropped_notes(&rendered, &map, n);
    }
    let body = rendered.strip_suffix('\n').unwrap_or(&rendered);
    let paths = map_saved_paths(&graph);
    let saved = savings_for(&graph, &paths);
    let text = with_savings_nl(product().name, body, saved.as_ref());
    (text, false)
}

/// The `… +N more directories` note for a dropped count, as
///  prints it: `N` in JavaScript number form, and the
/// singular only for exactly 1.
fn dropped_note(dropped: f64) -> String {
    let word = if dropped == 1.0 { "y" } else { "ies" };
    format!(
        "… +{} more director{word} not shown (raise max-dirs to see more)",
        js_number_text(dropped)
    )
}

/// Rewrites each dropped note for a fractional `max_dirs`. A fraction is kept:
/// `slice(0, 1.5)` shows one entry, and the note counts
/// `entries - 1.5`. `build_repo_map` counts whole entries,
/// so this swaps each whole-number note for the fractional one, in
/// render order (one per scope group, else the one global note).
/// ponytail: a string swap on the rendered text, kept inside this crate;
/// move the fraction into `MapOptions` if the CLI ever needs it too.
fn fractional_dropped_notes(rendered: &str, map: &RepoMap, max_dirs: f64) -> String {
    let groups: Vec<(usize, usize)> = match &map.scopes {
        Some(scopes) => scopes.iter().map(|s| (s.dirs.len(), s.dropped)).collect(),
        None => vec![(map.dirs.len(), map.dropped)],
    };
    let mut out = rendered.to_string();
    for (shown, dropped) in groups {
        if dropped == 0 {
            continue;
        }
        let fractional = ((shown + dropped) as f64 - max_dirs).max(0.0);
        out = out.replacen(&dropped_note(dropped as f64), &dropped_note(fractional), 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_string_follows_string_coercion() {
        assert_eq!(js_string(&serde_json::json!(5)), "5");
        assert_eq!(js_string(&serde_json::json!(1.5)), "1.5");
        assert_eq!(js_string(&serde_json::json!(true)), "true");
        assert_eq!(js_string(&serde_json::json!([1, null, "a"])), "1,,a");
        assert_eq!(js_string(&serde_json::json!({"a": 1})), "[object Object]");
        assert_eq!(js_string_arg(&serde_json::json!({"q": null}), "q"), "");
        assert_eq!(js_string_arg(&serde_json::json!({}), "q"), "");
    }

    #[test]
    fn depth_arg_ignores_strings_other_than_all_and_full() {
        assert_eq!(
            parse_depth_arg(&serde_json::json!({"depth": "ALL"})),
            Depth(Some(1))
        );
        assert_eq!(
            parse_depth_arg(&serde_json::json!({"depth": "2"})),
            Depth(Some(1))
        );
        assert_eq!(
            parse_depth_arg(&serde_json::json!({"depth": "all"})),
            Depth(None)
        );
        assert_eq!(
            parse_depth_arg(&serde_json::json!({"depth": 2.9})),
            Depth(Some(2))
        );
        assert_eq!(
            parse_depth_arg(&serde_json::json!({"depth": 0.5})),
            Depth(Some(1))
        );
    }

    #[test]
    fn dropped_note_prints_a_fraction_like_javascript() {
        assert_eq!(
            dropped_note(1.5),
            "… +1.5 more directories not shown (raise max-dirs to see more)"
        );
        assert_eq!(
            dropped_note(1.0),
            "… +1 more directory not shown (raise max-dirs to see more)"
        );
    }

    #[test]
    fn unknown_tool_reports_the_raw_name_as_an_error() {
        let (text, is_error) = dispatch(
            "sieve_nope",
            "sieve_nope",
            &Value::Null,
            Path::new("."),
            Path::new("."),
        );
        assert_eq!(text, "unknown tool: sieve_nope");
        assert!(is_error);
    }

    #[test]
    fn find_code_requires_a_query() {
        let (text, is_error) = find_code(&serde_json::json!({}), Path::new("."), Path::new("."));
        assert_eq!(
            text,
            format!("{} requires a query", product().tool("find_code"))
        );
        assert!(is_error);
    }

    fn capped_dir(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("sieve-s0c-{tag}-{}", std::process::id()));
        let ctx = root.join("ctx");
        std::fs::create_dir_all(ctx.join(".graph")).expect("make dir");
        std::fs::write(ctx.join(".graph").join("wiring.json"), "{}").expect("write wiring");
        (root, ctx)
    }

    #[test]
    fn test_s0_daemon_tool_returns_capped_error_over_cap() {
        let (root, ctx) = capped_dir("tool");
        set_test_wiring_cap(Some(1));
        let args = serde_json::json!({"query": "x", "symbol": "x", "pattern": "x", "file": "a"});
        for tool in [
            "find_code",
            "file_api",
            "trace_calls",
            "find_all",
            "repo_map",
        ] {
            let (text, is_error) = call(&product().tool(tool), &args, &root, &ctx);
            assert!(is_error, "{tool}");
            assert!(text.contains("over the size cap"), "{tool}: {text}");
            assert!(text.contains("2 bytes"), "{tool}: {text}");
            assert!(text.contains("cap is 1 bytes"), "{tool}: {text}");
            assert!(text.contains("SIEVE_WIRING_CAP_BYTES"), "{tool}: {text}");
        }
        set_test_wiring_cap(None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_s0_check_freshness_skips_build_over_cap() {
        let (root, ctx) = capped_dir("fresh");
        set_test_wiring_cap(Some(1));
        // The root holds no source; a build would give a stale or ok report.
        let (text, is_error) = check_freshness(&root, &ctx);
        assert!(is_error);
        assert!(text.contains("over the size cap"), "{text}");
        set_test_wiring_cap(None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_s3_check_freshness_lookup_equals_full() {
        let root = std::env::temp_dir().join(format!("sieve-s3t4-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("make dir");
        std::fs::write(root.join("a.ts"), "export function a() {}\n").expect("write a");
        std::fs::write(root.join("b.ts"), "export function b() {}\n").expect("write b");
        let ctx = root.join(product().context_dir_name());
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).expect("build graph");
        // A drifted file, so the text is not only the in-sync line.
        std::fs::write(root.join("a.ts"), "export function a2() {}\n").expect("edit a");

        let lookup = sieve_parse::refresh::lookup_path(&ctx);
        assert!(lookup.exists());
        let with_lookup = check_freshness(&root, &ctx);
        std::fs::remove_file(&lookup).expect("delete lookup");
        let without_lookup = check_freshness(&root, &ctx);

        assert_eq!(with_lookup, without_lookup);
        assert!(with_lookup.0.contains("behind"), "{}", with_lookup.0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_s0_cap_env_bad_value_falls_back() {
        assert_eq!(parse_wiring_cap(None), DEFAULT_WIRING_CAP);
        assert_eq!(parse_wiring_cap(Some("abc")), DEFAULT_WIRING_CAP);
        assert_eq!(parse_wiring_cap(Some("-5")), DEFAULT_WIRING_CAP);
        assert_eq!(parse_wiring_cap(Some("")), DEFAULT_WIRING_CAP);
        assert_eq!(parse_wiring_cap(Some("1024")), 1024);
    }

    #[test]
    fn test_s0_daemon_why_returns_capped_error_over_cap() {
        let (root, ctx) = capped_dir("why");
        set_test_wiring_cap(Some(1));
        let args = serde_json::json!({"symbol": "x"});
        let (text, is_error) = call(&product().tool("why"), &args, &root, &ctx);
        assert!(is_error);
        assert!(text.contains("over the size cap"), "{text}");
        set_test_wiring_cap(None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_s0_daemon_workspace_child_over_cap_is_not_parsed() {
        let (root, ctx) = capped_dir("ws");
        // The parent has no wiring; the child wiring is garbage over the cap.
        std::fs::remove_file(ctx.join(".graph").join("wiring.json")).expect("rm parent wiring");
        let ws = sieve_core::workspace::Workspace {
            version: 1,
            children: vec!["kid".to_string()],
        };
        std::fs::write(
            sieve_core::workspace::workspace_path(&ctx),
            serde_json::to_string(&ws).expect("ws json"),
        )
        .expect("write ws");
        let kid = root
            .join("kid")
            .join(product().context_dir_name())
            .join(".graph");
        std::fs::create_dir_all(&kid).expect("kid dir");
        std::fs::write(kid.join("wiring.json"), "not json at all").expect("kid wiring");
        set_test_wiring_cap(Some(1));
        for tool in ["find_all", "trace_calls", "repo_map", "check_freshness"] {
            let args = serde_json::json!({"pattern": "x", "symbol": "x"});
            let (text, is_error) = call(&product().tool(tool), &args, &root, &ctx);
            assert!(is_error, "{tool}");
            assert!(text.starts_with("kid: "), "{tool}: {text}");
            assert!(text.contains("over the size cap"), "{tool}: {text}");
        }
        set_test_wiring_cap(None);
        std::fs::remove_dir_all(&root).ok();
    }
}
