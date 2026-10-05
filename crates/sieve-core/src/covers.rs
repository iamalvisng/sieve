//! The `covers:` backfill: every concept node's frontmatter gets a list of the
//! symbols its `sources` claim, with an exact `file:span` (see the
//! `deep-tier.md` note section 2.5). Sieve never writes a concept node body or
//! a summary; this module only splices one frontmatter key into text the
//! `--deep` pass already wrote.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;

use crate::cards::{kind_str, span_start};
use crate::collate::collate;
use crate::concept::{split_frontmatter, SourceRef};
use crate::wiring::{Kind, Node};
use crate::write::write_atomic;

/// The frontmatter fields `write_covers` reads, before it decides whether a
/// file is a concept node. Every other field, and every other byte of the
/// frontmatter, passes through untouched.
#[derive(Debug, Default, Deserialize)]
struct Frontmatter {
    slug: Option<String>,
    #[serde(default)]
    sources: Vec<SourceRef>,
}

/// Groups every non-file node by its source path, sorted by span start then
/// name, matching `writeCovers`'s `symbolsByPath`.
fn group_symbols_by_path(nodes: &[Node]) -> HashMap<&str, Vec<&Node>> {
    let mut by_path: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in nodes {
        if node.kind == Kind::File {
            continue;
        }
        by_path.entry(node.path.as_str()).or_default().push(node);
    }
    for list in by_path.values_mut() {
        list.sort_by(|a, b| {
            span_start(&a.span)
                .cmp(&span_start(&b.span))
                .then_with(|| collate(&a.name, &b.name))
        });
    }
    by_path
}

/// One `covers:` entry, ready to render.
struct CoverEntry {
    symbol: String,
    kind: String,
    at: String,
}

/// Builds the `covers` list for one concept node's deduped, byte-sorted
/// source paths.
fn build_covers<'a>(
    sources: &[SourceRef],
    symbols_by_path: &HashMap<&'a str, Vec<&'a Node>>,
) -> Vec<CoverEntry> {
    let mut paths: Vec<&str> = sources.iter().map(|s| s.path.as_str()).collect();
    // Byte order, not ICU: 's plain `.sort` on the deduped
    // path set.
    paths.sort_unstable();
    paths.dedup();

    let mut covers = Vec::new();
    for path in paths {
        let Some(symbols) = symbols_by_path.get(path) else {
            continue;
        };
        for node in symbols {
            covers.push(CoverEntry {
                symbol: node.name.clone(),
                kind: kind_str(node.kind),
                at: format!("{}:{}", node.path, node.span),
            });
        }
    }
    covers
}

/// The fold width of a covers value: js-yaml `lineWidth` 80 minus the
/// indent of 6 (`covers` is level 1, the list item
/// level 2, the entry key level 3).
const FOLD_WIDTH: usize = 74;

/// The indent of a folded line under an entry key.
const FOLD_INDENT: &str = "      ";

/// Writes one single-line covers scalar as js-yaml 3.15 `writeScalar` does:
/// double-quoted when a character is not printable,
/// folded (`>-`) when the text is longer than `FOLD_WIDTH` and does not
/// start with a space, else plain or single-quoted (`chooseScalarStyle`).
/// A folded value holds newlines and its own indent.
fn yaml_scalar(s: &str) -> String {
    if s.chars().any(|c| c != '\n' && !is_yaml_printable(c)) {
        return format!("\"{}\"", escape_double(s));
    }
    let foldable = s
        .split('\n')
        .any(|l| l.chars().count() > FOLD_WIDTH && !l.starts_with(' '));
    if foldable || s.contains('\n') {
        return block_scalar(s, foldable);
    }
    if needs_quote(s) {
        format!("'{}'", s.replace('\'', "''"))
    } else {
        s.to_string()
    }
}

/// A literal (`|`) or folded (`>`) block scalar: js-yaml `blockHeader`,
/// `indentString`, `dropEndingNewline` and `foldString`
/// (`133-157`, `394-444`).
fn block_scalar(s: &str, fold: bool) -> String {
    let indicator = if s.trim_start_matches('\n').starts_with(' ') {
        "2"
    } else {
        ""
    };
    let clip = s.ends_with('\n');
    let keep = clip && (s.ends_with("\n\n") || s == "\n");
    let chomp = if keep {
        "+"
    } else if clip {
        ""
    } else {
        "-"
    };
    let text = if fold { fold_string(s) } else { s.to_string() };
    let mut body = String::new();
    for seg in text.split_inclusive('\n') {
        if seg != "\n" {
            body.push_str(FOLD_INDENT);
        }
        body.push_str(seg);
    }
    if body.ends_with('\n') {
        body.pop();
    }
    format!("{}{indicator}{chomp}\n{body}", if fold { '>' } else { '|' })
}

/// js-yaml `foldString`: folds each line, and adds
/// one LF between two lines that are not more-indented.
fn fold_string(s: &str) -> String {
    let fold = |l: &str| fold_line(&l.chars().collect::<Vec<_>>(), FOLD_WIDTH).join("\n");
    let first_end = s.find('\n').unwrap_or(s.len());
    let mut out = fold(&s[..first_end]);
    let mut prev_more = s.starts_with('\n') || s.starts_with(' ');
    let mut rest = &s[first_end..];
    while !rest.is_empty() {
        let after = rest.trim_start_matches('\n');
        let prefix = &rest[..rest.len() - after.len()];
        let end = after.find('\n').unwrap_or(after.len());
        let line = &after[..end];
        rest = &after[end..];
        let more = line.starts_with(' ');
        out.push_str(prefix);
        if !prev_more && !more && !line.is_empty() {
            out.push('\n');
        }
        out.push_str(&fold(line));
        prev_more = more;
    }
    out
}

/// js-yaml `escapeString` and `ESCAPE_SEQUENCES`.
/// A Rust `char` is a whole surrogate pair.
fn escape_double(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let n = c as u32;
        match c {
            '\0' => out.push_str("\\0"),
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0B}' => out.push_str("\\v"),
            '\u{0C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\u{1B}' => out.push_str("\\e"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{85}' => out.push_str("\\N"),
            '\u{A0}' => out.push_str("\\_"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            _ if is_yaml_printable(c) => out.push(c),
            _ if n <= 0xFF => out.push_str(&format!("\\x{n:02X}")),
            _ if n <= 0xFFFF => out.push_str(&format!("\\u{n:04X}")),
            _ => out.push_str(&format!("\\U{n:08X}")),
        }
    }
    out
}

/// js-yaml `foldLine`: greedy break at a space that
/// precedes a non-space. A line with no such space stays whole.
fn fold_line(line: &[char], width: usize) -> Vec<String> {
    let text = |a: usize, b: usize| line[a..b].iter().collect::<String>();
    if line.first().is_none_or(|c| *c == ' ') {
        return vec![text(0, line.len())];
    }
    let (mut start, mut curr) = (0, 0);
    let mut out = Vec::new();
    for next in 0..line.len().saturating_sub(1) {
        if line[next] != ' ' || line[next + 1] == ' ' {
            continue;
        }
        if next - start > width {
            let end = if curr > start { curr } else { next };
            out.push(text(start, end));
            start = end + 1;
        }
        curr = next;
    }
    if line.len() - start > width && curr > start {
        out.push(text(start, curr));
        out.push(text(curr + 1, line.len()));
    } else {
        out.push(text(start, line.len()));
    }
    out
}

/// js-yaml's `DEPRECATED_BOOLEANS_SYNTAX`.
const DEPRECATED_BOOLEANS: [&str; 16] = [
    "y", "Y", "yes", "Yes", "YES", "on", "On", "ON", "n", "N", "no", "No", "NO", "off", "Off",
    "OFF",
];

/// js-yaml `isWhitespace`: space and tab.
fn is_yaml_white(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// js-yaml `isPrintable`.
fn is_yaml_printable(c: char) -> bool {
    let n = c as u32;
    (0x20..=0x7E).contains(&n)
        || ((0xA1..=0xD7FF).contains(&n) && n != 0x2028 && n != 0x2029)
        || ((0xE000..=0xFFFD).contains(&n) && n != 0xFEFF)
    // No 0x10000.. range: js-yaml tests UTF-16 units, so an astral
    // character is a surrogate pair, not printable, and is double-quoted.
}

/// js-yaml `isPlainSafeFirst`.
fn is_plain_safe_first(c: char) -> bool {
    is_yaml_printable(c) && !is_yaml_white(c) && !"-?:,[]{}#&*!|=>'\"%@`".contains(c)
}

/// js-yaml `isPlainSafe`: `prev` is the character
/// before `c`, if any.
fn is_plain_safe(c: char, prev: Option<char>) -> bool {
    let prev_is_ns = |p: char| is_yaml_printable(p) && !is_yaml_white(p);
    is_yaml_printable(c) && !",[]{}:".contains(c) && (c != '#' || prev.is_some_and(prev_is_ns))
}

fn needs_quote(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return true; // empty string
    };
    if DEPRECATED_BOOLEANS.contains(&s) {
        return true;
    }
    let last = s.chars().last().unwrap_or(first);
    if !is_plain_safe_first(first) || is_yaml_white(last) {
        return true;
    }
    let mut prev = None;
    for c in s.chars() {
        if !is_plain_safe(c, prev) {
            return true;
        }
        prev = Some(c);
    }
    resolves_to_another_type(s)
}

/// True when js-yaml's implicit resolvers read `s` as null, bool, int,
/// float, timestamp or merge (`type/*.js`). Such a string needs quotes to stay a
/// string. `NaN`, `Infinity` and `inf` are not in the list.
fn resolves_to_another_type(s: &str) -> bool {
    matches!(
        s,
        "~" | "null"
            | "Null"
            | "NULL"
            | "true"
            | "True"
            | "TRUE"
            | "false"
            | "False"
            | "FALSE"
            | "<<"
    ) || resolves_to_int(s.as_bytes())
        || resolves_to_float(s.as_bytes())
        || resolves_to_timestamp(s.as_bytes())
}

/// Port of the two timestamp patterns: a date `YYYY-MM-DD`, or a date and time
/// with an optional fraction and zone.
fn resolves_to_timestamp(d: &[u8]) -> bool {
    // Takes `min..=max` ASCII digits from `i`; returns the new index.
    let digits = |i: usize, min: usize, max: usize| {
        let n = d[i.min(d.len())..]
            .iter()
            .take(max)
            .take_while(|c| c.is_ascii_digit())
            .count();
        (n >= min).then_some(i + n)
    };
    let eat = |i: usize, c: u8| (d.get(i) == Some(&c)).then_some(i + 1);
    let Some(i) = digits(0, 4, 4)
        .and_then(|i| eat(i, b'-'))
        .and_then(|i| digits(i, 1, 2))
        .and_then(|i| eat(i, b'-'))
        .and_then(|i| digits(i, 1, 2))
    else {
        return false;
    };
    // The date-only pattern needs two-digit month and day.
    if d.len() == 10
        && d[5..7].iter().all(u8::is_ascii_digit)
        && d[8..].iter().all(u8::is_ascii_digit)
    {
        return true;
    }
    let is_blank = |c: &u8| *c == b' ' || *c == b'\t';
    let i = if matches!(d.get(i), Some(b'T' | b't')) {
        i + 1
    } else {
        let n = d[i.min(d.len())..]
            .iter()
            .take_while(|c| is_blank(c))
            .count();
        if n == 0 {
            return false;
        }
        i + n
    };
    let Some(mut i) = digits(i, 1, 2)
        .and_then(|i| eat(i, b':'))
        .and_then(|i| digits(i, 2, 2))
        .and_then(|i| eat(i, b':'))
        .and_then(|i| digits(i, 2, 2))
    else {
        return false;
    };
    if d.get(i) == Some(&b'.') {
        i += 1;
        i += d[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    }
    if i == d.len() {
        return true;
    }
    i += d[i..].iter().take_while(|c| is_blank(c)).count();
    if d.get(i) == Some(&b'Z') {
        return i + 1 == d.len();
    }
    let Some(i) = d
        .get(i)
        .filter(|c| matches!(c, b'-' | b'+'))
        .and_then(|_| digits(i + 1, 1, 2))
    else {
        return false;
    };
    if i == d.len() {
        return true;
    }
    eat(i, b':')
        .and_then(|i| digits(i, 2, 2))
        .is_some_and(|i| i == d.len())
}

/// Port of `resolveYamlInteger`.
fn resolves_to_int(d: &[u8]) -> bool {
    let max = d.len();
    if max == 0 {
        return false;
    }
    let mut i = 0;
    if d[i] == b'-' || d[i] == b'+' {
        i += 1;
    }
    let at = |i: usize| d.get(i).copied();
    if at(i) == Some(b'0') {
        if i + 1 == max {
            return true;
        }
        i += 1;
        let digits_ok = |from: usize, ok: fn(u8) -> bool| {
            let mut has = false;
            for &c in &d[from..] {
                if c == b'_' {
                    continue;
                }
                if !ok(c) {
                    return false;
                }
                has = true;
            }
            has && d[max - 1] != b'_'
        };
        return match at(i) {
            Some(b'b') => digits_ok(i + 1, |c| c == b'0' || c == b'1'),
            Some(b'x') => digits_ok(i + 1, |c| c.is_ascii_hexdigit()),
            _ => digits_ok(i, |c| (b'0'..=b'7').contains(&c)),
        };
    }
    // Base 10 (not 0) or base 60. A leading `_` is not a number.
    if at(i) == Some(b'_') {
        return false;
    }
    let mut has_digits = false;
    let mut ch = None;
    while i < max {
        let c = d[i];
        ch = Some(c);
        if c == b'_' {
            i += 1;
            continue;
        }
        if c == b':' {
            break;
        }
        if !c.is_ascii_digit() {
            return false;
        }
        has_digits = true;
        i += 1;
    }
    if !has_digits || ch == Some(b'_') {
        return false;
    }
    if ch != Some(b':') {
        return true;
    }
    // Base 60: `(:[0-5]?[0-9])+` to the end.
    let mut rest = &d[i..];
    while !rest.is_empty() {
        if rest[0] != b':' {
            return false;
        }
        rest = match (rest.get(1), rest.get(2)) {
            (Some(a), Some(b)) if (b'0'..=b'5').contains(a) && b.is_ascii_digit() => &rest[3..],
            (Some(a), _) if a.is_ascii_digit() => &rest[2..],
            _ => return false,
        };
    }
    true
}

/// Port of `YAML_FLOAT_PATTERN` plus the trailing-`_` check.
fn resolves_to_float(d: &[u8]) -> bool {
    if d.last() == Some(&b'_') {
        return false;
    }
    let sign_len = usize::from(matches!(d.first(), Some(b'-' | b'+')));
    let unsigned = &d[sign_len..];
    // `.inf` forms, then `.nan` forms.
    if matches!(unsigned, b".inf" | b".Inf" | b".INF") {
        return true;
    }
    if sign_len == 0 && matches!(d, b".nan" | b".NaN" | b".NAN") {
        return true;
    }
    let skip = |from: usize, ok: fn(u8) -> bool| {
        let mut i = from;
        while i < d.len() && ok(d[i]) {
            i += 1;
        }
        i
    };
    let digit_or_us = |c: u8| c.is_ascii_digit() || c == b'_';
    // `(?:[eE][-+]?[0-9]+)?` then the end, from `i`.
    let exponent_then_end = |i: usize| {
        if i == d.len() {
            return true;
        }
        if !matches!(d[i], b'e' | b'E') {
            return false;
        }
        let mut j = i + 1;
        if j < d.len() && matches!(d[j], b'-' | b'+') {
            j += 1;
        }
        let end = skip(j, |c| c.is_ascii_digit());
        end > j && end == d.len()
    };
    // Alt 2: `\.[0-9_]+(?:[eE][-+]?[0-9]+)?` (no sign).
    if sign_len == 0 && d.first() == Some(&b'.') {
        let end = skip(1, digit_or_us);
        return end > 1 && exponent_then_end(end);
    }
    // Alt 1: `[-+]?(?:0|[1-9][0-9_]*)(?:\.[0-9_]*)?(?:[eE][-+]?[0-9]+)?`.
    let mut i = sign_len;
    match d.get(i) {
        Some(b'0') => i += 1,
        Some(c) if (b'1'..=b'9').contains(c) => i = skip(i + 1, digit_or_us),
        _ => return false,
    }
    if d.get(i) == Some(&b'.') {
        i = skip(i + 1, digit_or_us);
    }
    if exponent_then_end(i) {
        return true;
    }
    // Alt 3: `[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*`.
    let mut i = sign_len;
    if !d.get(i).is_some_and(u8::is_ascii_digit) {
        return false;
    }
    i = skip(i + 1, digit_or_us);
    if d.get(i) != Some(&b':') {
        return false;
    }
    while d.get(i) == Some(&b':') {
        i = match (d.get(i + 1), d.get(i + 2)) {
            (Some(a), Some(b)) if (b'0'..=b'5').contains(a) && b.is_ascii_digit() => i + 3,
            (Some(a), _) if a.is_ascii_digit() => i + 2,
            _ => return false,
        };
    }
    d.get(i) == Some(&b'.') && skip(i + 1, digit_or_us) == d.len()
}

/// Renders the new `covers:` block, appended last in the frontmatter.
fn render_covers_block(covers: &[CoverEntry]) -> Vec<String> {
    if covers.is_empty() {
        return vec!["covers: []".to_string()];
    }
    let mut lines = vec!["covers:".to_string()];
    for entry in covers {
        lines.push(format!("  - symbol: {}", yaml_scalar(&entry.symbol)));
        lines.push(format!("    kind: {}", yaml_scalar(&entry.kind)));
        lines.push(format!("    at: {}", yaml_scalar(&entry.at)));
    }
    lines
}

/// Removes an existing top-level `covers:` block from a frontmatter's
/// lines, in place: the `covers:` line and every following line that is
/// blank or indented, up to the next top-level key or the end.
fn strip_covers_block(lines: &[&str]) -> Vec<String> {
    let Some(start) = lines.iter().position(|line| line.starts_with("covers:")) else {
        return lines.iter().map(|s| s.to_string()).collect();
    };
    let mut end = start + 1;
    while end < lines.len() {
        let line = lines[end];
        if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') {
            end += 1;
        } else {
            break;
        }
    }
    lines[..start]
        .iter()
        .chain(lines[end..].iter())
        .map(|s| s.to_string())
        .collect()
}

/// Splices a new `covers:` block into a frontmatter's YAML text, removing
/// any previous one first, and appending the new one last.
fn splice_covers(yaml: &str, covers: &[CoverEntry]) -> String {
    let lines: Vec<&str> = yaml.split('\n').collect();
    let mut kept = strip_covers_block(&lines);
    kept.extend(render_covers_block(covers));
    kept.join("\n")
}

/// Backfills the `covers:` block into every concept node under
/// `context_dir`, from the current wiring graph's nodes.
///
/// Skips a file with no `.md` extension, `INDEX.md`, and any file whose
/// frontmatter carries no non-empty `slug` — the same guard that keeps a
/// root-level per-file wiring card out (issue #261).
/// Writes a file back only when the new bytes differ, so a build over an
/// unchanged tree touches nothing. With no concept files this is a no-op.
pub fn write_covers(context_dir: &Path, nodes: &[Node]) -> io::Result<usize> {
    let symbols_by_path = group_symbols_by_path(nodes);

    let entries = match fs::read_dir(context_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };

    let mut enriched = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".md") || name == "INDEX.md" {
            continue;
        }
        let path = entry.path();
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Some((yaml, body)) = split_frontmatter(&text) else {
            continue;
        };
        let fm: Frontmatter = serde_yaml_ng::from_str(yaml).unwrap_or_default();
        let has_slug = fm.slug.as_deref().is_some_and(|s| !s.is_empty());
        if !has_slug {
            continue;
        }

        let covers = build_covers(&fm.sources, &symbols_by_path);
        let new_yaml = splice_covers(yaml, &covers);
        let new_text = format!("---\n{new_yaml}\n---\n{body}");
        if new_text != text {
            write_atomic(&path, new_text.as_bytes())?;
        }
        enriched += 1;
    }
    Ok(enriched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::{Kind, Node, Origin, SummaryState};

    /// A temp dir unique per call, that removes itself on drop, even if
    /// the test panics before it reaches its own cleanup line.
    fn unique_temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    fn node(id: &str, name: &str, kind: Kind, path: &str, span: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: true,
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

    fn sample_nodes() -> Vec<Node> {
        vec![
            node("app.ts", "app.ts", Kind::File, "app.ts", "L1-L1"),
            node("app.ts#run", "run", Kind::Function, "app.ts", "L5-L10"),
            node("app.ts#helper", "helper", Kind::Function, "app.ts", "L1-L3"),
        ]
    }

    #[test]
    fn a_node_with_no_prior_covers_gets_the_block_appended_last() {
        let dir = unique_temp_dir("append");
        fs::write(
            dir.join("widget.md"),
            "---\nname: Widget\nslug: widget\ntype: concept\nsources:\n  - path: app.ts\n    hash: aaaa\n---\nbody\n",
        )
        .expect("write concept node");

        let enriched = write_covers(&dir, &sample_nodes()).expect("write covers succeeds");
        assert_eq!(enriched, 1);

        let text = fs::read_to_string(dir.join("widget.md")).expect("read node");
        assert_eq!(
            text,
            "---\nname: Widget\nslug: widget\ntype: concept\nsources:\n  - path: app.ts\n    hash: aaaa\ncovers:\n  - symbol: helper\n    kind: function\n    at: 'app.ts:L1-L3'\n  - symbol: run\n    kind: function\n    at: 'app.ts:L5-L10'\n---\nbody\n"
        );
    }

    #[test]
    fn a_node_with_an_old_covers_block_gets_it_replaced_and_every_other_byte_kept() {
        let dir = unique_temp_dir("replace");
        fs::write(
            dir.join("widget.md"),
            "---\nname: Widget\nslug: widget\nsources:\n  - path: app.ts\n    hash: aaaa\ncovers:\n  - symbol: stale\n    kind: function\n    at: 'app.ts:L1-L1'\ngenerator:\n  version: 1\n---\n<!-- context:generated:start -->\nbody text\n<!-- context:generated:end -->\n",
        )
        .expect("write concept node");

        write_covers(&dir, &sample_nodes()).expect("write covers succeeds");

        let text = fs::read_to_string(dir.join("widget.md")).expect("read node");
        assert_eq!(
            text,
            "---\nname: Widget\nslug: widget\nsources:\n  - path: app.ts\n    hash: aaaa\ngenerator:\n  version: 1\ncovers:\n  - symbol: helper\n    kind: function\n    at: 'app.ts:L1-L3'\n  - symbol: run\n    kind: function\n    at: 'app.ts:L5-L10'\n---\n<!-- context:generated:start -->\nbody text\n<!-- context:generated:end -->\n"
        );
    }

    /// Each row is a string and whether js-yaml 3.15 (through
    /// gray-matter) wrote it quoted. The flags come
    /// from running that library on each string. `NaN`, `Infinity` and
    /// `inf` stay plain; `y`, `.nan` and `0x1f` take quotes.
    #[test]
    fn test_p2_37_dv13_scalar_quoting_matches_js_yaml() {
        let rows: [(&str, bool); 141] = [
            ("NaN", false),
            ("Infinity", false),
            ("inf", false),
            ("nan", false),
            (".nan", true),
            (".inf", true),
            ("-.inf", true),
            ("+.inf", true),
            (".NaN", true),
            (".Inf", true),
            ("-.INF", true),
            ("yes", true),
            ("no", true),
            ("on", true),
            ("off", true),
            ("y", true),
            ("n", true),
            ("~", true),
            ("null", true),
            ("Null", true),
            ("NULL", true),
            ("true", true),
            ("True", true),
            ("TRUE", true),
            ("TrUe", false),
            ("false", true),
            ("False", true),
            ("1", true),
            ("1.5", true),
            ("1e3", true),
            ("1E3", true),
            ("1_000", true),
            ("0x1f", true),
            ("0o7", false),
            ("0b1", true),
            ("017", true),
            ("08", false),
            ("1:30", true),
            ("1:30:00", true),
            ("2001-01-01", true),
            ("2001-01-01T00:00:00Z", true),
            ("<<", true),
            ("-", true),
            ("- a", true),
            ("a b", false),
            ("a: b", true),
            ("a #b", true),
            ("a#b", false),
            ("?x", true),
            ("@a", true),
            ("`a", true),
            ("1_", false),
            ("_1", false),
            ("0.", true),
            (".5", true),
            ("-0", true),
            ("+1", true),
            ("1.2.3", false),
            ("e3", false),
            ("0e0", true),
            ("1e", false),
            ("Foo", false),
            ("x1e3", false),
            ("!a", true),
            ("%a", true),
            ("&a", true),
            ("*a", true),
            ("|a", true),
            (">a", true),
            ("'a", true),
            ("\"a", true),
            ("a'b", false),
            ("a\"b", false),
            ("a b ", true),
            (" a", true),
            ("$", false),
            ("a,b", true),
            ("a[b", true),
            ("a{b", true),
            ("é", false),
            ("日本", false),
            ("x y", false),
            ("a\\b", false),
            ("=", true),
            ("=a", true),
            ("0", true),
            ("-1", true),
            ("1.", true),
            ("1.e3", true),
            ("1e+3", true),
            ("1e-3", true),
            ("-1.5e3", true),
            ("0.5", true),
            ("00", true),
            ("0_0", true),
            ("-.5", true),
            ("+.5", false),
            ("+0x1", true),
            ("0xZ", false),
            ("a:", true),
            ("a:b", true),
            ("a: ", true),
            ("a?", false),
            ("?", true),
            ("? a", true),
            ("-a", true),
            ("--", true),
            ("---", true),
            ("a - b", false),
            ("[a]", true),
            ("{a}", true),
            ("a]", true),
            ("a}", true),
            ("#", true),
            ("#a", true),
            ("a #", true),
            ("  ", true),
            ("1,000", true),
            ("1.5.", false),
            ("0b", false),
            ("0b2", false),
            ("0o8", false),
            ("0x", false),
            ("0X1F", false),
            ("-0x1", true),
            ("- 1", true),
            ("<", false),
            ("<<<", false),
            ("a<<", false),
            ("Y", true),
            ("N", true),
            ("null ", true),
            ("~a", false),
            ("a~", false),
            ("Infinity.", false),
            ("-Infinity", true),
            ("+Infinity", false),
            ("-NaN", true),
            ("INF", false),
            ("Inf", false),
            ("NAN", false),
        ];
        for (text, quoted) in rows {
            assert_eq!(needs_quote(text), quoted, "quote decision for {text:?}");
        }
        assert_eq!(yaml_scalar("NaN"), "NaN");
        assert_eq!(yaml_scalar("it's: x"), "'it''s: x'");
    }

    /// A symbol named `NaN` or `Infinity` lands in `covers:` unquoted, as
    /// js-yaml output.
    #[test]
    fn test_p2_37_dv13_nan_and_infinity_symbols_are_written_plain() {
        let dir = unique_temp_dir("dv13");
        fs::write(
            dir.join("foo.md"),
            "---\nname: Foo\nslug: foo\nsources:\n  - path: a.ts\n---\nbody\n",
        )
        .expect("write concept node");
        let nodes = vec![
            node("a.ts", "a.ts", Kind::File, "a.ts", "L1-L1"),
            node("a.ts#NaN", "NaN", Kind::Function, "a.ts", "L1-L3"),
            node("a.ts#Infinity", "Infinity", Kind::Function, "a.ts", "L4-L6"),
        ];
        write_covers(&dir, &nodes).expect("write covers succeeds");
        let text = fs::read_to_string(dir.join("foo.md")).expect("read node");
        assert!(text.contains("  - symbol: NaN\n    kind: function\n    at: 'a.ts:L1-L3'\n"));
        assert!(text.contains("  - symbol: Infinity\n"));
    }

    #[test]
    fn entries_sort_by_span_start_then_name() {
        let dir = unique_temp_dir("sort");
        fs::write(
            dir.join("widget.md"),
            "---\nslug: widget\nsources:\n  - path: app.ts\n    hash: a\n---\nbody\n",
        )
        .expect("write concept node");
        let nodes = vec![
            node("app.ts#b", "b", Kind::Function, "app.ts", "L1-L2"),
            node("app.ts#a", "a", Kind::Function, "app.ts", "L1-L2"),
            node("app.ts#z", "z", Kind::Function, "app.ts", "L3-L4"),
        ];

        write_covers(&dir, &nodes).expect("write covers succeeds");
        let text = fs::read_to_string(dir.join("widget.md")).expect("read node");
        let symbols: Vec<&str> = text
            .lines()
            .filter(|l| l.trim_start().starts_with("- symbol:"))
            .map(|l| l.trim_start().trim_start_matches("- symbol: "))
            .collect();
        // Same span start (L1-L2): "a" collates before "b". Then the later
        // span (L3-L4) gives "z" last.
        assert_eq!(symbols, vec!["a", "b", "z"]);
    }

    #[test]
    fn a_concept_whose_sources_name_no_file_gets_an_empty_covers_list() {
        let dir = unique_temp_dir("empty");
        fs::write(
            dir.join("widget.md"),
            "---\nslug: widget\nsources:\n  - path: missing.ts\n    hash: a\n---\nbody\n",
        )
        .expect("write concept node");

        write_covers(&dir, &sample_nodes()).expect("write covers succeeds");
        let text = fs::read_to_string(dir.join("widget.md")).expect("read node");
        assert!(text.contains("covers: []\n"));
    }

    #[test]
    fn a_root_level_card_with_no_slug_is_skipped() {
        let dir = unique_temp_dir("skip");
        let original = "---\n{}\n---\n# app.ts\n\nbody\n";
        fs::write(dir.join("app.md"), original).expect("write wiring card");

        let enriched = write_covers(&dir, &sample_nodes()).expect("write covers succeeds");
        assert_eq!(enriched, 0);
        let text = fs::read_to_string(dir.join("app.md")).expect("read card");
        assert_eq!(text, original);
    }

    #[test]
    fn a_second_run_changes_no_bytes() {
        let dir = unique_temp_dir("stable");
        fs::write(
            dir.join("widget.md"),
            "---\nname: Widget\nslug: widget\nsources:\n  - path: app.ts\n    hash: a\n---\nbody\n",
        )
        .expect("write concept node");

        write_covers(&dir, &sample_nodes()).expect("first write covers succeeds");
        let first = fs::read(dir.join("widget.md")).expect("read after first run");
        let first_mtime = fs::metadata(dir.join("widget.md"))
            .expect("stat after first run")
            .modified()
            .expect("mtime supported");

        // Force the clock forward enough to detect a spurious rewrite: if
        // the second run's bytes matched the first, `write_atomic` never
        // runs, and the mtime stays put.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_covers(&dir, &sample_nodes()).expect("second write covers succeeds");
        let second = fs::read(dir.join("widget.md")).expect("read after second run");
        let second_mtime = fs::metadata(dir.join("widget.md"))
            .expect("stat after second run")
            .modified()
            .expect("mtime supported");

        assert_eq!(first, second);
        assert_eq!(first_mtime, second_mtime);
    }

    #[test]
    fn yaml_scalar_quotes_a_colon_bearing_at_value() {
        assert_eq!(yaml_scalar("src/app.ts:L5-L10"), "'src/app.ts:L5-L10'");
    }

    #[test]
    fn yaml_scalar_leaves_a_plain_symbol_name_unquoted() {
        assert_eq!(yaml_scalar("run"), "run");
    }

    #[test]
    fn yaml_scalar_quotes_a_reserved_literal_and_a_number() {
        assert_eq!(yaml_scalar("true"), "'true'");
        assert_eq!(yaml_scalar("123"), "'123'");
    }

    #[test]
    fn with_zero_concept_files_write_covers_is_a_no_op() {
        let dir = unique_temp_dir("noop");
        let enriched = write_covers(&dir, &sample_nodes()).expect("write covers succeeds");
        assert_eq!(enriched, 0);
    }

    /// Expected values come from js-yaml 3.15.2 `safeDump({covers: [{at:
    /// value}]})`, run in node. A folded value shows its text after `>-`,
    /// indented 6 spaces.
    #[test]
    fn test_p2_37_scalar_fold_and_double_quote_match_js_yaml() {
        let a72 = "a".repeat(72);
        let a71 = "a".repeat(71);
        let words = "word ".repeat(20);
        let words90 = format!("{}tail", &words[..90]);
        let rows: Vec<(String, String)> = vec![
            (format!("{a72}:L1"), format!(">-\n      {a72}:L1")),
            (format!("{a71}:L1"), format!("'{a71}:L1'")),
            (
                words90,
                ">-\n      word word word word word word word word word word word word word word word\n      word word word tail"
                    .to_string(),
            ),
            ("a".repeat(80) + " b", format!(">-\n      {}\n      b", "a".repeat(80))),
            ("😀Handler".into(), "\"\\U0001F600Handler\"".into()),
            ("a\tb".into(), "\"a\\tb\"".into()),
            ("a\u{a0}b".into(), "\"a\\_b\"".into()),
            ("a\u{85}b".into(), "\"a\\Nb\"".into()),
            ("a\u{1}b".into(), "\"a\\x01b\"".into()),
            ("a\"b\\c\u{2028}".into(), "\"a\\\"b\\\\c\\L\"".into()),
            ("\u{feff}a".into(), "\"\\uFEFFa\"".into()),
            ("x\u{7f}y".into(), "\"x\\x7Fy\"".into()),
            ("é\u{9f}".into(), "\"é\\x9F\"".into()),
        ];
        for (text, want) in rows {
            assert_eq!(yaml_scalar(&text), want, "scalar for {text:?}");
        }
    }

    /// Expected values come from js-yaml 3.15.2 run in
    /// node: a value with LF is a `|` literal or a `>` fold with blank lines.
    #[test]
    fn test_p2_37_lf_values_and_edge_spaces_match_js_yaml() {
        let b40 = "b".repeat(40);
        let a40 = "a".repeat(40);
        let w80 = "word ".repeat(20);
        let rows: Vec<(String, String)> = vec![
            (
                format!(" {}", "a".repeat(75)),
                format!("' {}'", "a".repeat(75)),
            ),
            (
                format!("{}  {}", "w".repeat(70), "v".repeat(10)),
                format!(">-\n      {} \n      {}", "w".repeat(70), "v".repeat(10)),
            ),
            (
                format!("{}  {}  {}", "x".repeat(36), "y".repeat(36), "z".repeat(10)),
                format!(
                    ">-\n      {} \n      {}  {}",
                    "x".repeat(36),
                    "y".repeat(36),
                    "z".repeat(10)
                ),
            ),
            (
                w80[..79].to_string() + " ",
                format!(">-\n      {}\n      word ", "word ".repeat(15).trim_end()),
            ),
            (
                "a".repeat(80) + " ",
                format!(">-\n      {} ", "a".repeat(80)),
            ),
            ("a\nb".into(), "|-\n      a\n      b".into()),
            ("a\n".into(), "|\n      a".into()),
            ("\n".into(), "|+\n".into()),
            (" a\nb".into(), "|2-\n       a\n      b".into()),
            ("a\n\nb".into(), "|-\n      a\n\n      b".into()),
            ("a\n\n".into(), "|+\n      a\n".into()),
            ("\na".into(), "|-\n\n      a".into()),
            (
                format!("{a40} {b40}\nz"),
                format!(">-\n      {a40}\n      {b40}\n\n      z"),
            ),
        ];
        for (text, want) in rows {
            assert_eq!(yaml_scalar(&text), want, "scalar for {text:?}");
        }
    }

    /// A long `at` value is written folded, as js-yaml writes it.
    #[test]
    fn test_p2_37_a_long_at_value_is_folded_in_the_covers_block() {
        let dir = unique_temp_dir("fold");
        let path = format!("src/{}/app.ts", "d".repeat(60));
        fs::write(
            dir.join("foo.md"),
            format!("---\nname: Foo\nslug: foo\nsources:\n  - path: {path}\n---\nbody\n"),
        )
        .expect("write concept node");
        let nodes = vec![node("x#run", "run", Kind::Function, &path, "L1-L3")];
        write_covers(&dir, &nodes).expect("write covers succeeds");
        let text = fs::read_to_string(dir.join("foo.md")).expect("read node");
        let want = format!("  - symbol: run\n    kind: function\n    at: >-\n      {path}:L1-L3\n");
        assert!(text.contains(&want), "{text}");
    }

    /// An emoji symbol name is double-quoted, as js-yaml writes it.
    #[test]
    fn test_p2_37_an_emoji_symbol_name_is_double_quoted_in_the_covers_block() {
        let dir = unique_temp_dir("emoji");
        fs::write(
            dir.join("foo.md"),
            "---\nname: Foo\nslug: foo\nsources:\n  - path: a.ts\n---\nbody\n",
        )
        .expect("write concept node");
        let nodes = vec![node("a.ts#h", "😀Handler", Kind::Function, "a.ts", "L1-L3")];
        write_covers(&dir, &nodes).expect("write covers succeeds");
        let text = fs::read_to_string(dir.join("foo.md")).expect("read node");
        assert!(
            text.contains("  - symbol: \"\\U0001F600Handler\"\n    kind: function\n"),
            "{text}"
        );
    }
}
