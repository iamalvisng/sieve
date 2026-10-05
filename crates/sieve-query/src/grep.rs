//! `grep`: a regex search over indexed files, grouped by enclosing symbol.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use sieve_core::{collate::collate, Graph, Kind, Node};
use thiserror::Error;

use crate::relations::compute_in_degree;

/// The default cap on the number of hits `grep` returns.
pub const DEFAULT_MAX_HITS: usize = 300;

/// The hard limit, in UTF-16 code units, on one hit's stored text.
pub const MAX_HIT_TEXT: usize = 160;

/// The flags a `grep` call runs with.
#[derive(Debug, Clone)]
pub struct GrepOptions {
    /// Match the pattern without regard to letter case.
    pub ignore_case: bool,
    /// Treat the pattern as a literal string, not a regex.
    pub fixed: bool,
    /// Search only files under this path prefix, when set.
    pub in_prefix: Option<String>,
    /// The most hits `grep` returns before it starts counting overflow.
    pub max_hits: usize,
}

/// The symbol a hit group sits under.
#[derive(Debug, Clone, Serialize)]
pub struct GrepSymbolRef {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub span: String,
}

/// One matched line, trimmed and capped.
#[derive(Debug, Clone, Serialize)]
pub struct GrepHit {
    pub line: u32,
    pub text: String,
}

/// Every hit under one symbol, or under a file's module level.
#[derive(Debug, Clone, Serialize)]
pub struct GrepGroup {
    pub symbol: Option<GrepSymbolRef>,
    pub path: String,
    #[serde(rename = "inDegree")]
    pub in_degree: usize,
    pub hits: Vec<GrepHit>,
}

/// The count of hits and files a `grep` run could not fit or read.
#[derive(Debug, Clone, Serialize)]
pub struct GrepTruncated {
    pub files: usize,
    pub hits: usize,
}

/// The full result of one `grep` run.
#[derive(Debug, Clone, Serialize)]
pub struct GrepResult {
    pub pattern: String,
    #[serde(rename = "filesSearched")]
    pub files_searched: usize,
    #[serde(rename = "totalHits")]
    pub total_hits: usize,
    pub groups: Vec<GrepGroup>,
    pub truncated: GrepTruncated,
}

/// Renders the `PrefixNotIndexed` message, with the scope list only when
/// there is more than one scope to name.
fn prefix_not_indexed_message(prefix: &str, scopes: &[String]) -> String {
    let clause = if scopes.len() > 1 {
        format!(" (scopes here: {})", scopes.join(" \u{b7} "))
    } else {
        String::new()
    };
    format!(
        "nothing is indexed under {prefix}/{clause} \u{2014} try {} map",
        sieve_core::product().name
    )
}

/// Every way `grep` can fail.
#[derive(Debug, Error)]
pub enum GrepError {
    /// The pattern does not compile as a regex.
    #[error("invalid pattern \"{pattern}\": {message} \u{2014} fix the pattern, or add --fixed")]
    InvalidPattern { pattern: String, message: String },
    /// The `--in` prefix matches no indexed node.
    #[error("{}", prefix_not_indexed_message(prefix, scopes))]
    PrefixNotIndexed { prefix: String, scopes: Vec<String> },
    /// A file could not be read for a reason worth reporting up front.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Escapes the regex metacharacters V8 escapes, and no others.
///
/// V8's `escapeRegExp` leaves `-` and `/` alone.
pub fn escape_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        if matches!(
            c,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The `regress` parse-error texts and the V8 `SyntaxError` reason each
/// one stands for. The first column is a prefix of the `regress` text.
/// Two `regress` texts cover more than one V8 reason; `v8_reason` splits
/// those on the pattern.
const V8_REASONS: &[(&str, &str)] = &[
    ("Invalid group modifier", "Invalid group"),
    (
        "Invalid quantifier",
        "numbers out of order in {} quantifier",
    ),
    ("Invalid braced quantifier", "Nothing to repeat"),
    ("Quantifier not allowed here", "Invalid quantifier"),
    (
        "Range values reversed",
        "Range out of order in character class",
    ),
    ("Invalid atom character", "Nothing to repeat"),
    ("Unbalanced bracket", "Unterminated character class"),
    ("Incomplete escape", "\\ at end of pattern"),
    (
        "Duplicate capture group name",
        "Duplicate capture group name",
    ),
    (
        "Backreference to invalid named capture group",
        "Invalid named capture referenced",
    ),
    (
        "Invalid named backreference syntax",
        "Invalid named reference",
    ),
];

/// True when `pattern` closes a group it never opened, which V8 reports
/// as `Unmatched ')'`. Otherwise an unbalanced pattern has an open group
/// with no close, which V8 reports as `Unterminated group`.
fn closes_an_unopened_group(pattern: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => in_class = true,
            ']' => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Picks V8's reason for one `regress` parse error on `pattern`. An
/// unmapped case falls back to the `regress` text.
fn v8_reason(pattern: &str, raw: &str) -> String {
    if raw.starts_with("Unbalanced parenthesis") {
        return if closes_an_unopened_group(pattern) {
            "Unmatched ')'".to_string()
        } else {
            "Unterminated group".to_string()
        };
    }
    if raw.starts_with("Invalid token at named capture group identifier") {
        // `(?<n` and `(?<1>a)` fail on the name; `(?` and `(?x` fail on
        // the group syntax itself.
        return if pattern.contains("(?<") {
            "Invalid capture group name".to_string()
        } else {
            "Invalid group".to_string()
        };
    }
    V8_REASONS
        .iter()
        .find(|(prefix, _)| raw.starts_with(prefix))
        .map(|(_, reason)| reason.to_string())
        .unwrap_or_else(|| raw.to_string())
}

/// Maps a `regress` parse error to V8's `SyntaxError.message` wording,
/// the text `sieve grep` prints for a bad pattern.
pub fn v8_error_message(pattern: &str, err: &regress::Error) -> String {
    format!(
        "Invalid regular expression: /{pattern}/: {}",
        v8_reason(pattern, &err.to_string())
    )
}

/// Compiles the user pattern like JS: `new RegExp(source,
/// "i" | "")`, with no `u` flag, through the `regress` crate (ledger
/// 2026-09-30, "the regress crate runs the grep user pattern").
fn compile_pattern(pattern: &str, opts: &GrepOptions) -> Result<regress::Regex, GrepError> {
    let source = if opts.fixed {
        escape_regex(pattern)
    } else {
        pattern.to_string()
    };
    let flags = if opts.ignore_case { "i" } else { "" };
    regress::Regex::with_flags(&source, flags).map_err(|err| GrepError::InvalidPattern {
        pattern: pattern.to_string(),
        message: v8_error_message(pattern, &err),
    })
}

/// Slices `s` to at most `max` UTF-16 code units, with no ellipsis.
fn truncate_utf16(s: &str, max: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    if units.len() <= max {
        return s.to_string();
    }
    String::from_utf16_lossy(&units[..max])
}

/// Strips a leading `./` (repeatedly), maps a platform separator to `/`,
/// and strips every trailing `/` from a `--in` prefix. Matches Sieve's
/// `normalizePathPrefix`: a leading slash is not
/// special on its own — only a wholly-slash string (`"/"`, `"///"`) ends
/// up empty, through the trailing-slash strip.
fn normalize_path_prefix(prefix: &str) -> String {
    let mut out = if cfg!(windows) {
        prefix.replace('\\', "/")
    } else {
        prefix.to_string()
    };
    while let Some(rest) = out.strip_prefix("./") {
        out = rest.to_string();
    }
    out.trim_end_matches('/').to_string()
}

/// Renders a scope prefix the way the `scopeLabel` does.
fn scope_label(prefix: &str) -> String {
    if prefix.is_empty() {
        "(root)".to_string()
    } else {
        format!("{prefix}/")
    }
}

/// Segment-aware prefix match, mirroring `pathUnderPrefix`
///: an empty prefix matches every path.
fn path_under_prefix(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Parses a node's `L<start>-L<end>` span into a pair of line numbers.
fn parse_span(span: &str) -> Option<(u32, u32)> {
    let rest = span.strip_prefix('L')?;
    let (start, rest) = rest.split_once("-L")?;
    Some((start.parse().ok()?, rest.parse().ok()?))
}

/// Finds the innermost symbol enclosing `line` among `symbols`, sorted by
/// start. The greatest start wins; a same-start tie keeps the later node.
fn enclosing_symbol<'a>(symbols: &[(&'a Node, u32, u32)], line: u32) -> Option<&'a Node> {
    let mut best = None;
    for (node, start, end) in symbols {
        if *start > line {
            break;
        }
        if *end >= line {
            best = Some(*node);
        }
    }
    best
}

fn read_file_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return None;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (chunks, _) = bytes[2..].as_chunks::<2>();
        let units: Vec<u16> = chunks.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Runs a `grep` search over `graph`'s indexed files, on disk under
/// `repo_root`.
pub fn grep_graph(
    graph: &Graph,
    repo_root: &Path,
    pattern: &str,
    opts: &GrepOptions,
) -> Result<GrepResult, GrepError> {
    let regex = compile_pattern(pattern, opts)?;

    let in_degree = compute_in_degree(graph);

    let prefix = match &opts.in_prefix {
        Some(raw) => {
            let normalized = normalize_path_prefix(raw);
            // An empty prefix (from a bare `/`) means "match everything",
            // the same as no `--in` at all — never an indexed check, never
            // an error, even on an empty graph (`assertPrefixIndexed`'s
            // `!prefix` short-circuit).
            if !normalized.is_empty() {
                let indexed = graph
                    .nodes
                    .iter()
                    .any(|n| path_under_prefix(&n.path, &normalized));
                if !indexed {
                    let scopes: Vec<String> = graph
                        .meta
                        .scopes
                        .iter()
                        .map(|s| scope_label(&s.prefix))
                        .collect();
                    return Err(GrepError::PrefixNotIndexed {
                        prefix: normalized,
                        scopes,
                    });
                }
            }
            Some(normalized)
        }
        None => None,
    };

    let file_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .filter(|n| match &prefix {
            Some(p) => path_under_prefix(&n.path, p),
            None => true,
        })
        .collect();

    let mut symbols_by_path: HashMap<&str, Vec<(&Node, u32, u32)>> = HashMap::new();
    for node in &graph.nodes {
        if node.kind == Kind::File {
            continue;
        }
        if let Some((start, end)) = parse_span(&node.span) {
            symbols_by_path
                .entry(node.path.as_str())
                .or_default()
                .push((node, start, end));
        }
    }
    for symbols in symbols_by_path.values_mut() {
        symbols.sort_by_key(|(_, start, _)| *start);
    }

    let mut groups: Vec<GrepGroup> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();
    let mut total_hits = 0usize;
    let mut truncated_hits = 0usize;
    let mut truncated_files = 0usize;

    for file_node in &file_nodes {
        let full_path = repo_root.join(&file_node.path);
        let Some(text) = read_file_text(&full_path) else {
            truncated_files += 1;
            continue;
        };
        let symbols = symbols_by_path.get(file_node.path.as_str());
        for (idx, raw_line) in text.split('\n').enumerate() {
            if regex.find(raw_line).is_none() {
                continue;
            }
            let line = (idx + 1) as u32;
            if total_hits >= opts.max_hits {
                truncated_hits += 1;
                continue;
            }
            let hit_text = truncate_utf16(raw_line.trim(), MAX_HIT_TEXT);

            let symbol_node = symbols.and_then(|s| enclosing_symbol(s, line));
            let key = match symbol_node {
                Some(node) => node.id.clone(),
                None => format!("file:{}", file_node.path),
            };
            let group_pos = *group_index.entry(key).or_insert_with(|| {
                let group = match symbol_node {
                    Some(node) => {
                        let name = node
                            .id
                            .split_once('#')
                            .map(|(_, rest)| rest.to_string())
                            .unwrap_or_else(|| node.name.clone());
                        GrepGroup {
                            symbol: Some(GrepSymbolRef {
                                id: node.id.clone(),
                                name,
                                kind: node.kind,
                                path: node.path.clone(),
                                span: node.span.clone(),
                            }),
                            path: node.path.clone(),
                            in_degree: *in_degree.get(&node.id).unwrap_or(&0),
                            hits: Vec::new(),
                        }
                    }
                    None => GrepGroup {
                        symbol: None,
                        path: file_node.path.clone(),
                        in_degree: 0,
                        hits: Vec::new(),
                    },
                };
                groups.push(group);
                groups.len() - 1
            });
            groups[group_pos].hits.push(GrepHit {
                line,
                text: hit_text,
            });
            total_hits += 1;
        }
    }

    groups.sort_by(|a, b| {
        b.in_degree
            .cmp(&a.in_degree)
            .then_with(|| collate(&a.path, &b.path))
    });

    Ok(GrepResult {
        pattern: pattern.to_string(),
        files_searched: file_nodes.len(),
        total_hits,
        groups,
        truncated: GrepTruncated {
            files: truncated_files,
            hits: truncated_hits,
        },
    })
}

/// Renders a [`Kind`] the way it serializes to JSON: a lowercase word.
fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "file",
        Kind::Class => "class",
        Kind::Function => "function",
        Kind::Method => "method",
        Kind::Interface => "interface",
        Kind::Type => "type",
        Kind::Enum => "enum",
        Kind::Struct => "struct",
        Kind::Trait => "trait",
        Kind::Module => "module",
        Kind::Constant => "constant",
        Kind::Variable => "variable",
    }
}

fn format_group(g: &GrepGroup) -> String {
    let links = format!(
        "{} {} in",
        g.in_degree,
        if g.in_degree == 1 { "link" } else { "links" }
    );
    let header = match &g.symbol {
        Some(sym) => format!(
            "{}  \u{b7} {links}",
            sieve_core::voice::row(&sym.name, kind_str(sym.kind), &sym.path, Some(&sym.span))
        ),
        None => {
            let name = g.path.rsplit('/').next().unwrap_or(&g.path);
            format!("{name}  module  {}  \u{b7} {links}", g.path)
        }
    };
    let hit_lines: Vec<String> = g
        .hits
        .iter()
        .map(|h| format!("  {}: {}", h.line, h.text))
        .collect();
    format!("{header}\n{}", hit_lines.join("\n"))
}

/// Renders a [`GrepResult`] as the plain-text `grep` report.
pub fn format_grep_result(r: &GrepResult) -> String {
    let files_hit: std::collections::HashSet<&str> =
        r.groups.iter().map(|g| g.path.as_str()).collect();
    let header = format!(
        "grep \"{}\" \u{b7} {} in {} across {} \u{b7} searched {}",
        r.pattern,
        sieve_core::voice::count(r.total_hits, "hit"),
        sieve_core::voice::count(r.groups.len(), "symbol"),
        sieve_core::voice::count(files_hit.len(), "file"),
        sieve_core::voice::count(r.files_searched, "file")
    );

    let mut note_parts = Vec::new();
    if r.truncated.hits > 0 {
        let s = if r.truncated.hits == 1 { "" } else { "s" };
        note_parts.push(format!("{} more hit{s} beyond the cap", r.truncated.hits));
    }
    if r.truncated.files > 0 {
        let s = if r.truncated.files == 1 { "" } else { "s" };
        note_parts.push(format!("{} indexed file{s} unreadable", r.truncated.files));
    }
    let head = if note_parts.is_empty() {
        header
    } else {
        format!(
            "{header}\ntruncated: {} \u{b7} narrow with --in, or use a shorter pattern",
            note_parts.join(", ")
        )
    };

    let mut parts: Vec<String> = vec![head, String::new()];
    for group in &r.groups {
        parts.push(format!("{}\n", format_group(group)));
    }
    let joined = parts.join("\n");
    collapse_trailing_newlines(&joined)
}

fn collapse_trailing_newlines(s: &str) -> String {
    let trimmed = s.trim_end_matches('\n');
    format!("{trimmed}\n")
}

/// Renders the zero-hit note Sieve prints to stderr when `grep` finds
/// nothing.
pub fn zero_hit_note(r: &GrepResult) -> String {
    let mut note = format!(
        "no hits for \"{}\" in {} indexed files \u{2014} try a bare symbol name or a short \
substring, or run {} ask \"<question>\". Use raw grep -rn only for files the index does not hold",
        r.pattern,
        r.files_searched,
        sieve_core::product().name
    );
    if r.truncated.files > 0 {
        let s = if r.truncated.files == 1 { "" } else { "s" };
        note.push_str(&format!(
            ". {} indexed file{s} could not be read \u{2014} run {} build",
            r.truncated.files,
            sieve_core::product().name
        ));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Meta, Node, Origin, SummaryState};

    fn file_node(path: &str, end: u32) -> Node {
        Node {
            id: path.to_string(),
            name: path.to_string(),
            kind: Kind::File,
            owner: None,
            path: path.to_string(),
            span: format!("L1-L{end}"),
            signature: None,
            exported: false,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    fn symbol_node(id: &str, name: &str, kind: Kind, path: &str, start: u32, end: u32) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: format!("L{start}-L{end}"),
            signature: None,
            exported: false,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

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

    fn default_opts() -> GrepOptions {
        GrepOptions {
            ignore_case: false,
            fixed: false,
            in_prefix: None,
            max_hits: DEFAULT_MAX_HITS,
        }
    }

    #[test]
    fn escape_regex_leaves_dash_and_slash_alone() {
        assert_eq!(
            escape_regex("a.b*c(d)[e]-f/g"),
            "a\\.b\\*c\\(d\\)\\[e\\]-f/g"
        );
    }

    #[test]
    fn v8_error_message_matches_the_unterminated_character_class_golden() {
        let err = regress::Regex::new("([").expect_err("([ does not compile as a regex");
        assert_eq!(
            v8_error_message("([", &err),
            "Invalid regular expression: /([/: Unterminated character class"
        );
    }

    #[test]
    fn crlf_line_does_not_match_a_dollar_anchor_before_the_carriage_return() {
        let re = regress::Regex::new("foo$").expect("valid regex");
        assert!(re.find("foo\r").is_none());
        assert!(re.find("foo").is_some());
    }

    /// The pattern and the `-i` flag one `basic-grep-regex.txt` line names.
    /// The line shape is `grep '<pattern>' [-i] --in src/regex-cases.ts`.
    fn parse_regex_query(args: &str) -> (String, bool) {
        let rest = args
            .strip_prefix("grep '")
            .expect("query starts with grep '");
        let (pattern, tail) = rest.split_once('\'').expect("closing quote");
        (pattern.to_string(), tail.contains(" -i "))
    }

    /// With `SIEVE_BLESS=1`, writes `got` to the golden file.
    fn bless_regex_golden(dir: &Path, id: &str, ext: &str, got: &str) {
        if std::env::var("SIEVE_BLESS").is_ok_and(|v| v == "1") {
            std::fs::write(dir.join(format!("{id}.{ext}")), got).expect("write golden");
        }
    }

    fn read_regex_golden(dir: &Path, id: &str, ext: &str) -> String {
        std::fs::read_to_string(dir.join(format!("{id}.{ext}")))
            .unwrap_or_else(|_| panic!("read golden {id}.{ext}"))
    }

    /// P1-12, P3-16: the user pattern runs as `new RegExp(source, "i" | "")`
    /// does, with no `u` flag. Every golden comes from a recorded run over
    /// `tests/inputs/grep-regex/regex-cases.ts` alone. An exit-0 case compares
    /// stdout; an exit-1 case compares the `sieve: invalid pattern` line.
    #[test]
    fn test_p1_12_p3_16_grep_regex_engine_matches_golden() {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let root = Path::new(&manifest).join("../..");
        let list = std::fs::read_to_string(root.join("tests/inputs/queries/basic-grep-regex.txt"))
            .expect("read basic-grep-regex.txt");
        let golden_dir = root.join("tests/fixtures/basic.expected/grep-regex");
        let case_file = root.join("tests/inputs/grep-regex/regex-cases.ts");
        let case_text = std::fs::read(&case_file).expect("read regex-cases.ts");
        let line_count = case_text.iter().filter(|b| **b == b'\n').count() as u32;

        let dir = tempdir("regex-engine");
        std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
        std::fs::write(dir.join("src/regex-cases.ts"), &case_text).expect("copy case file");
        let mut graph = empty_graph();
        graph
            .nodes
            .push(file_node("src/regex-cases.ts", line_count));

        let mut ran = 0;
        let mut failures = Vec::new();
        for line in list.lines() {
            let Some((id, args)) = line.split_once('\t') else {
                continue;
            };
            if id.starts_with('#') {
                continue;
            }
            ran += 1;
            let (pattern, ignore_case) = parse_regex_query(args);
            let opts = GrepOptions {
                ignore_case,
                in_prefix: Some("src/regex-cases.ts".to_string()),
                ..default_opts()
            };
            let want_exit = read_regex_golden(&golden_dir, id, "exit.txt");
            match grep_graph(&graph, &dir, &pattern, &opts) {
                Ok(result) => {
                    let got = if result.total_hits == 0 {
                        String::new()
                    } else {
                        format_grep_result(&result)
                    };
                    bless_regex_golden(&golden_dir, id, "stdout.txt", &got);
                    let want = read_regex_golden(&golden_dir, id, "stdout.txt");
                    if want_exit.trim() != "0" || got != want {
                        failures.push(format!("{id}: want exit {want_exit}\n{want}\ngot\n{got}"));
                    }
                }
                Err(GrepError::InvalidPattern { message, .. }) => {
                    let got = format!(
                        "sieve: invalid pattern \"{pattern}\": {message} \u{2014} fix the pattern, or add --fixed\n"
                    );
                    bless_regex_golden(&golden_dir, id, "stderr.txt", &got);
                    let want = read_regex_golden(&golden_dir, id, "stderr.txt");
                    if want_exit.trim() != "1" || got != want {
                        failures.push(format!("{id}: want exit {want_exit}\n{want}\ngot\n{got}"));
                    }
                }
                Err(other) => failures.push(format!("{id}: unexpected error {other}")),
            }
        }
        assert!(ran >= 24, "only {ran} regex-engine queries ran");
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }

    /// DEVIATION (ledger 2026-09-30, "the regress crate runs the grep user
    /// pattern"): with `-i`, `regress` folds `ſ` (U+017F) to `s` and `ı`
    /// (U+0131) to `i`. V8 without the `u` flag folds neither. This test
    /// states the current behaviour; it does not claim a match with V8.
    #[test]
    fn test_p1_12_p3_16_deviation_icase_folds_long_s_and_dotless_i() {
        let opts = GrepOptions {
            ignore_case: true,
            ..default_opts()
        };
        let long_s = compile_pattern("s", &opts).expect("s compiles");
        assert!(long_s.find("ſ").is_some());
        let dotless_i = compile_pattern("i", &opts).expect("i compiles");
        assert!(dotless_i.find("ı").is_some());
        // V8 and `regress` agree on the Kelvin sign: `k` does not match it.
        let kelvin = compile_pattern("k", &opts).expect("k compiles");
        assert!(kelvin.find("\u{212A}").is_none());
    }

    /// DEVIATION (ledger 2026-09-30, "the regress crate runs the grep user
    /// pattern"): `.` matches an astral character as one character. V8
    /// without the `u` flag sees two UTF-16 units, so `^.$` fails there.
    /// This test states the current behaviour; it does not claim parity.
    #[test]
    fn test_p1_12_p3_16_deviation_dot_matches_one_astral_char() {
        let re = compile_pattern("^.$", &default_opts()).expect("^.$ compiles");
        assert!(re.find("\u{1F600}").is_some());
    }

    #[test]
    fn test_p1_12_p3_16_v8_reason_splits_unbalanced_parenthesis_on_the_pattern() {
        assert!(closes_an_unopened_group("(a))"));
        assert!(!closes_an_unopened_group("((a)"));
        assert!(!closes_an_unopened_group("[)]"));
        assert!(!closes_an_unopened_group("\\)"));
        assert_eq!(
            v8_reason("(?<n", "Invalid token at named capture group identifier"),
            "Invalid capture group name"
        );
        assert_eq!(
            v8_reason("(?x)", "Invalid token at named capture group identifier"),
            "Invalid group"
        );
        assert_eq!(
            v8_reason("zz", "Some new regress text"),
            "Some new regress text"
        );
    }

    #[test]
    fn truncate_utf16_slices_to_160_code_units_with_no_ellipsis() {
        let long = "a".repeat(200);
        let sliced = truncate_utf16(&long, MAX_HIT_TEXT);
        assert_eq!(sliced.encode_utf16().count(), MAX_HIT_TEXT);
        assert!(!sliced.contains('…'));
    }

    #[test]
    fn the_cap_counts_overflow_exactly_across_two_files() {
        let mut graph = empty_graph();
        graph.nodes.push(file_node("a.txt", 3));
        graph.nodes.push(file_node("b.txt", 3));
        let dir = tempdir("overflow");
        std::fs::write(dir.join("a.txt"), "x\nx\nx\n").expect("write a");
        std::fs::write(dir.join("b.txt"), "x\nx\nx\n").expect("write b");
        let opts = GrepOptions {
            max_hits: 4,
            ..default_opts()
        };
        let result = grep_graph(&graph, &dir, "x", &opts).expect("grep runs");
        assert_eq!(result.total_hits, 4);
        assert_eq!(result.truncated.hits, 2);
    }

    /// Writes `a.ts`: one long hit line, then 300 short hit lines. The
    /// long line trims to 214 UTF-16 units, so the 160-unit cut lands
    /// inside its run of `a`.
    fn write_301_hits(dir: &Path) {
        let mut text = format!("hit(); // hit {}\n", "a".repeat(200));
        for _ in 0..300 {
            text.push_str("hit(); // hit\n");
        }
        std::fs::write(dir.join("a.ts"), text).expect("write a.ts");
    }

    #[test]
    fn test_p1_12_p3_19_default_cap_keeps_300_hits_and_cuts_text_at_160_units() {
        // Every literal below is the recorded output on the same tree:
        // `sieve grep hit` and `sieve grep hit --json` over a.ts (301
        // matching lines, the long one first) and b.ts (one line, no hit).
        let mut graph = empty_graph();
        graph.nodes.push(file_node("a.ts", 301));
        graph.nodes.push(file_node("b.ts", 1));
        let dir = tempdir("default-cap");
        write_301_hits(&dir);
        std::fs::write(dir.join("b.ts"), "export function b() { return 1; }\n").expect("write b");
        let result = grep_graph(&graph, &dir, "hit", &default_opts()).expect("grep runs");
        assert_eq!(result.total_hits, 300);
        assert_eq!(result.truncated.hits, 1);
        assert_eq!(result.truncated.files, 0);
        let first = &result.groups[0].hits[0].text;
        assert_eq!(first, &format!("hit(); // hit {}", "a".repeat(146)));
        assert_eq!(first.encode_utf16().count(), 160);
        assert_eq!(
            format_grep_result(&result).lines().next(),
            Some("grep \"hit\" \u{b7} 300 hits in 1 symbol across 1 file \u{b7} searched 2 files")
        );
    }

    #[test]
    fn test_p1_13_p3_19_truncation_note_is_line_two_and_counts_unreadable_files() {
        // The literal is the recorded line 2 on the same tree after
        // `rm b.ts` with no rebuild: the cap overflow and the missing
        // indexed file share one note.
        let mut graph = empty_graph();
        graph.nodes.push(file_node("a.ts", 301));
        graph.nodes.push(file_node("b.ts", 1));
        let dir = tempdir("truncation-note");
        write_301_hits(&dir);
        let result = grep_graph(&graph, &dir, "hit", &default_opts()).expect("grep runs");
        assert_eq!(result.truncated.hits, 1);
        assert_eq!(result.truncated.files, 1);
        let text = format_grep_result(&result);
        assert_eq!(
            text.lines().nth(1),
            Some(
                "truncated: 1 more hit beyond the cap, 1 indexed file unreadable \u{b7} \
                 narrow with --in, or use a shorter pattern"
            )
        );
        assert_eq!(text.lines().nth(2), Some(""));
    }

    #[test]
    fn test_p1_15_zero_hit_note_pluralizes_the_unreadable_count() {
        let mut graph = empty_graph();
        graph.nodes.push(file_node("a.ts", 1));
        graph.nodes.push(file_node("b.ts", 1));
        let dir = tempdir("zero-hit-plural");
        let mut result = grep_graph(&graph, &dir, "zzz", &default_opts()).expect("grep runs");
        assert_eq!(result.truncated.files, 2);
        assert!(zero_hit_note(&result)
            .ends_with(". 2 indexed files could not be read \u{2014} run sieve build"));
        result.truncated.files = 1;
        assert!(zero_hit_note(&result)
            .ends_with(". 1 indexed file could not be read \u{2014} run sieve build"));
    }

    #[test]
    fn nested_symbol_grouping_picks_the_greatest_start() {
        let mut graph = empty_graph();
        graph.nodes.push(file_node("a.ts", 5));
        graph.nodes.push(symbol_node(
            "a.ts#Outer",
            "Outer",
            Kind::Class,
            "a.ts",
            1,
            5,
        ));
        graph.nodes.push(symbol_node(
            "a.ts#Outer.inner",
            "inner",
            Kind::Method,
            "a.ts",
            2,
            4,
        ));
        let dir = tempdir("nested");
        std::fs::write(
            dir.join("a.ts"),
            "class Outer {\n  inner() {\n    hit();\n  }\n}\n",
        )
        .expect("write file");
        let result = grep_graph(&graph, &dir, "hit", &default_opts()).expect("grep runs");
        assert_eq!(result.groups.len(), 1);
        assert_eq!(
            result.groups[0].symbol.as_ref().map(|s| s.name.as_str()),
            Some("Outer.inner")
        );
    }

    #[test]
    fn a_hit_with_no_enclosing_symbol_lands_in_the_module_level_group() {
        let mut graph = empty_graph();
        graph.nodes.push(file_node("a.ts", 3));
        graph.nodes.push(symbol_node(
            "a.ts#later",
            "later",
            Kind::Function,
            "a.ts",
            2,
            3,
        ));
        let dir = tempdir("no-symbol");
        std::fs::write(dir.join("a.ts"), "hit();\nfunction later() {\n}\n").expect("write file");
        let result = grep_graph(&graph, &dir, "hit", &default_opts()).expect("grep runs");
        assert_eq!(result.groups.len(), 1);
        assert!(result.groups[0].symbol.is_none());
        assert_eq!(result.groups[0].path, "a.ts");
    }

    /// A throwaway temp dir that removes itself on drop, even if the test
    /// panics before it reaches its own cleanup line.
    fn tempdir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    #[test]
    fn test_p1_41_normalize_path_prefix_matches_golden() {
        // Every case pinned from recorded runs of the path prefix rule.
        assert_eq!(normalize_path_prefix("./src/app/"), "src/app");
        assert_eq!(normalize_path_prefix("src/ap"), "src/ap");
        assert_eq!(normalize_path_prefix("/"), "");
        assert_eq!(normalize_path_prefix(""), "");
        // A leading slash is not special on its own — only a wholly-slash
        // string collapses to "", through the trailing-slash strip.
        assert_eq!(normalize_path_prefix("/src/app"), "/src/app");
        // "." alone never matches "./" and is left untouched, matching the
        // live oracle exactly (it does NOT normalize to "").
        assert_eq!(normalize_path_prefix("."), ".");
        // A repeated leading "./" strips every layer, not just one.
        assert_eq!(normalize_path_prefix("././src"), "src");
    }

    #[test]
    fn test_p1_41_normalize_path_prefix_backslash_is_platform_dependent() {
        // The path prefix rule only maps a backslash to `/` when
        // the running platform's separator is `\` (Windows) — a posix
        // filename may legitimately contain a literal backslash. Verified
        // live: `normalizePathPrefix("src\\app")` on a posix host returns
        // `"src\\app"` unchanged.
        let out = normalize_path_prefix("src\\app");
        if cfg!(windows) {
            assert_eq!(out, "src/app");
        } else {
            assert_eq!(out, "src\\app");
        }
    }

    #[test]
    fn test_p1_41_path_under_prefix_is_segment_aware() {
        assert!(path_under_prefix("src/app/x.ts", "src/app"));
        assert!(path_under_prefix("src/app", "src/app"));
        // "src/ap" must never match "src/app/x.ts" — the match is
        // segment-aware, not a bare string prefix.
        assert!(!path_under_prefix("src/app/x.ts", "src/ap"));
    }

    #[test]
    fn test_p1_42_empty_prefix_matches_every_path() {
        // An empty prefix (from a bare "/") means "match everything", the
        // same as no `--in` at all.
        assert!(path_under_prefix("src/app/x.ts", ""));
        assert!(path_under_prefix("", ""));
    }

    #[test]
    fn test_p1_42_in_filters_files_searched_before_grep_runs() {
        let mut graph = empty_graph();
        graph.nodes.push(file_node("src/app/a.ts", 1));
        graph.nodes.push(file_node("lib/b.ts", 1));
        let dir = tempdir("in-filters-before-search");
        std::fs::create_dir_all(dir.join("src/app")).expect("create src/app");
        std::fs::write(dir.join("src/app").join("a.ts"), "hit();\n").expect("write a.ts");
        std::fs::write(dir.join("b.ts"), "hit();\n").expect("write b.ts");
        let mut opts = default_opts();
        opts.in_prefix = Some("src/app".to_string());
        let result = grep_graph(&graph, &dir, "hit", &opts).expect("grep runs");
        // `filesSearched` itself is narrowed, not just the hit set:
        // (stored path) is outside the prefix and never counted.
        assert_eq!(result.files_searched, 1);
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].path, "src/app/a.ts");
    }

    #[test]
    fn test_p1_44_scope_label_renders_root_and_prefix() {
        assert_eq!(scope_label(""), "(root)");
        assert_eq!(scope_label("apps/api"), "apps/api/");
    }
}
