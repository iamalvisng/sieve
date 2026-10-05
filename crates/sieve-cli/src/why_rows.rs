//! The query-time rows of `sieve why` (F6, slice 2): reason comments, pinning
//! tests and the history of one symbol span. Every row attaches to the span
//! by its location, by a syntax-tree comment node or by a resolved call
//! edge. No row comes from a guess about prose. Every row shows its source
//! as `file:line`. Every text that reaches the output passes through
//! [`redact`] first.

use std::path::Path;

use sieve_core::wiring::{Confidence, Graph, Kind, Node, Relation};
use sieve_query::blast::strip_url_credentials;

/// The longest comment text of a row, in characters.
const COMMENT_MAX: usize = 160;
/// The longest subject text of a row, in characters.
const LINE_MAX: usize = 120;
/// The most test rows and comment rows without `--all`.
pub const TYPE_CAP: usize = 3;
/// A base64 run of this length or more, with a digit and one of `+/=`, is a
/// secret.
const B64_MIN: usize = 40;
/// The mark that replaces a secret.
const MARK: &str = "[redacted]";
/// The words that make an identifier a key name. The match is a substring
/// match, in any case.
const KEYS: [&str; 8] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "auth",
    "accountkey",
];
/// Known key prefixes with the shortest tail that makes a match.
const PREFIXES: [(&str, usize); 12] = [
    ("github_pat_", 8),
    ("ghp_", 8),
    ("gho_", 8),
    ("glpat-", 8),
    ("AKIA", 12),
    ("AIza", 8),
    ("sk-ant-", 8),
    ("sk_live_", 8),
    ("rk_live_", 8),
    ("sk-", 8),
    ("npm_", 36),
    ("hf_", 30),
];

// ---------------------------------------------------------------------------
// The redactor
// ---------------------------------------------------------------------------

/// Removes the ANSI escape codes and the other control characters. A tab
/// becomes a space.
fn strip_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                // A CSI sequence ends at a byte from 0x40 to 0x7e.
                Some('[') => {
                    chars.next();
                    for d in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&d) {
                            break;
                        }
                    }
                }
                // An OSC sequence ends at BEL or at ESC.
                Some(']') => {
                    chars.next();
                    for d in chars.by_ref() {
                        if d == '\u{7}' || d == '\u{1b}' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\t' {
            out.push(' ');
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Replaces each PEM block with the mark. An open block with no end
/// replaces the rest of the text.
fn strip_pem(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("-----BEGIN ") {
        out.push_str(&rest[..at]);
        out.push_str(MARK);
        let tail = &rest[at..];
        match tail.find("-----END ") {
            Some(e) => {
                let after = &tail[e + 9..];
                rest = after.find("-----").map_or("", |d| &after[d + 5..]);
            }
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// True for a byte of an identifier.
fn is_id(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The index of the first non-blank character at or after `i`.
fn skip_blanks(text: &str, i: usize) -> usize {
    i + (text[i..].len() - text[i..].trim_start().len())
}

/// The end of the non-space run that starts at `from`. A `;` or a `,` also
/// ends it.
fn run_end(text: &str, from: usize) -> usize {
    text[from..]
        .find(|c: char| c.is_whitespace() || c == ';' || c == ',')
        .map_or(text.len(), |p| from + p)
}

/// The byte range of the literal value after the key word that ends at `i`,
/// or `None`. A literal is a quoted string, or a non-space run after `=`.
/// After `:` only a quoted string counts, so `token: &str` stays. For an
/// `auth` key, `Basic <x>` and `Bearer <x>` count too.
fn value_span(text: &str, i: usize, auth: bool) -> Option<(usize, usize)> {
    let mut j = i;
    if text[j..].starts_with(['"', '\'']) {
        j += 1;
    }
    j = skip_blanks(text, j);
    let sep = text[j..].chars().next()?;
    if sep != ':' && sep != '=' {
        return None;
    }
    if sep == '=' && text[j + 1..].starts_with(['=', '>', '~']) {
        return None;
    }
    let v = skip_blanks(text, j + 1);
    let first = text[v..].chars().next()?;
    if first == '"' || first == '\'' {
        let end = text[v + 1..]
            .find(first)
            .map_or(text.len(), |p| v + 1 + p + 1);
        return Some((v, end));
    }
    if sep == '=' {
        let e = run_end(text, v);
        return (e > v).then_some((v, e));
    }
    let word_end = run_end(text, v);
    let scheme = text[v..word_end].to_ascii_lowercase();
    if auth && (scheme == "basic" || scheme == "bearer") {
        let s = skip_blanks(text, word_end);
        let e = run_end(text, s);
        return (e > s).then_some((s, e));
    }
    None
}

/// Hides the literal value of a key identifier (a name that holds
/// `password`, `secret`, `token`, `api_key` and so on) and the token after
/// `Bearer`. One pass, no copy per character.
fn strip_pairs(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if is_id(b[i]) {
            let start = i;
            while i < b.len() && is_id(b[i]) {
                i += 1;
            }
            let word = &text[start..i];
            out.push_str(word);
            let lower = word.to_ascii_lowercase();
            let span = if lower == "bearer" {
                let v = skip_blanks(text, i);
                let e = run_end(text, v);
                (v > i && e > v).then_some((v, e))
            } else if KEYS.iter().any(|k| lower.contains(k)) {
                value_span(text, i, lower.contains("auth"))
            } else {
                None
            };
            if let Some((keep, end)) = span {
                out.push_str(&text[i..keep]);
                out.push_str(MARK);
                i = end;
            }
            continue;
        }
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The length of a prefixed key at the start of `s`, or `None`.
fn prefixed_key(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let is_tail = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-');
    let (head, min) =
        if b.len() > 4 && b.starts_with(b"xox") && b[3].is_ascii_alphabetic() && b[4] == b'-' {
            (5, 8)
        } else {
            let (p, min) = PREFIXES.iter().find(|(p, _)| s.starts_with(p))?;
            (p.len(), *min)
        };
    let tail = b[head..].iter().take_while(|c| is_tail(**c)).count();
    (tail >= min).then_some(head + tail)
}

/// The length of a JWT at the start of `s`, or `None`. A JWT starts with
/// `eyJ` and holds a dot.
fn jwt_len(s: &str) -> Option<usize> {
    if !s.starts_with("eyJ") {
        return None;
    }
    let n = s
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        .count();
    (n >= 10 && s[..n].contains('.')).then_some(n)
}

/// Hides the keys with a known prefix and the JWTs. A match needs a
/// non-alphanumeric character, or the start, before it.
fn strip_prefixed(text: &str) -> String {
    let mut out = String::new();
    let mut prev_alnum = false;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let hit = if prev_alnum {
            None
        } else {
            prefixed_key(rest).or_else(|| jwt_len(rest))
        };
        if let Some(n) = hit {
            out.push_str(MARK);
            i += n;
            prev_alnum = false;
        } else {
            let Some(c) = rest.chars().next() else {
                break;
            };
            out.push(c);
            prev_alnum = c.is_ascii_alphanumeric();
            i += c.len_utf8();
        }
    }
    out
}

/// Hides each base64 run of 40 characters or more that holds a digit and one
/// of `+`, `/` and `=`. A long name or a plain path stays.
fn strip_base64(text: &str) -> String {
    let is_b64 = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=');
    let mut out = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        let digit = run.chars().any(|c| c.is_ascii_digit());
        let sign = run.chars().any(|c| matches!(c, '+' | '/' | '='));
        if run.len() >= B64_MIN && digit && sign {
            out.push_str(MARK);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if is_b64(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Removes the userinfo of every URL in `text`.
fn strip_urls(text: &str) -> String {
    text.split(' ')
        .map(strip_url_credentials)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The secret redactor. It runs on every comment and commit text before
/// output. It strips ANSI codes, PEM blocks, URL userinfo, the literal
/// value of a key identifier, `Bearer` and `Basic` tokens, known key
/// prefixes, JWTs and base64 runs.
pub fn redact(text: &str) -> String {
    let plain = strip_controls(text);
    let plain = strip_pem(&plain);
    let plain = strip_urls(&plain);
    let plain = strip_pairs(&plain);
    let plain = strip_prefixed(&plain);
    strip_base64(&plain)
}

/// Cuts `text` to 4 times `max` characters, redacts it, trims it, and cuts
/// it to `max` characters.
pub fn clean(text: &str, max: usize) -> String {
    let head: String = text.chars().take(max * 4).collect();
    let red = redact(&head);
    let t = red.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        let cut: String = t.chars().take(max).collect();
        format!("{cut}...")
    }
}

// ---------------------------------------------------------------------------
// Comments, from the syntax tree
// ---------------------------------------------------------------------------

/// True when `hay` holds `word` with no letter, digit or `_` on either
/// side.
fn has_word(hay: &str, word: &str) -> bool {
    let is_w = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(word).any(|(at, _)| {
        let before = hay[..at].chars().next_back();
        let after = hay[at + word.len()..].chars().next();
        !before.is_some_and(is_w) && !after.is_some_and(is_w)
    })
}

/// True when a comment carries a reason: one of the tags SAFETY, WHY, NOTE,
/// HACK, TODO and FIXME, or one of the words `because`, `so that` and
/// `never`.
fn is_reason(comment: &str) -> bool {
    let lower = comment.to_lowercase();
    ["SAFETY", "WHY", "NOTE", "HACK", "TODO", "FIXME"]
        .iter()
        .any(|w| has_word(comment, w))
        || ["because", "so that", "never"]
            .iter()
            .any(|w| has_word(&lower, w))
}

/// The 1-based first and last line of a node span.
fn span_of(n: &Node) -> Option<(usize, usize)> {
    let (a, b) = n.span.split_once('-')?;
    Some((
        a.trim_start_matches('L').parse().ok()?,
        b.trim_start_matches('L').parse().ok()?,
    ))
}

/// The comment rows of the span of `n`. A comment block comes from comment
/// nodes of the syntax tree. A file that cannot be read, or a language with
/// no parser, gives no row.
fn comment_rows(root: &Path, n: &Node) -> Vec<String> {
    let (Some((a, b)), Ok(text)) = (span_of(n), std::fs::read_to_string(root.join(&n.path))) else {
        return Vec::new();
    };
    sieve_parse::comments::comment_blocks(&n.path, &text, a, b)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, t)| is_reason(t))
        .map(|(line, t)| format!("comment  {}:{line}  {}", n.path, clean(&t, COMMENT_MAX)))
        .collect()
}

// ---------------------------------------------------------------------------
// Pinning tests, from the resolved call edges
// ---------------------------------------------------------------------------

/// True when `caller` is test code: a node of a test file, a Rust node
/// with a `#[test]` attribute, or a node inside a `#[cfg(test)]` module.
fn is_test_node(
    graph: &Graph,
    root: &Path,
    caller: &Node,
    test_path: impl Fn(&str) -> bool,
) -> bool {
    if test_path(&caller.path) {
        return true;
    }
    let Some((line, _)) = span_of(caller) else {
        return false;
    };
    let text = std::fs::read_to_string(root.join(&caller.path)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    // The attribute sits on the first line of the node or just above it.
    let marked = |start: usize, attr: &str| {
        (start.saturating_sub(3)..=start).any(|k| {
            lines
                .get(k)
                .is_some_and(|l| l.trim_start().starts_with(attr))
        })
    };
    marked(line.saturating_sub(1), "#[test]")
        || graph
            .nodes
            .iter()
            .filter(|m| m.kind == Kind::Module && m.path == caller.path)
            .filter_map(span_of)
            .filter(|(a, b)| *a <= line && line <= *b)
            .any(|(a, _)| marked(a.saturating_sub(1), "#[cfg(test)]"))
}

/// The test rows of `n`: the test nodes that a resolved call edge links to
/// it. An inferred (name-match) edge gives no row.
fn test_rows(
    graph: &Graph,
    root: &Path,
    n: &Node,
    test_path: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut found: Vec<(&str, usize, &str)> = graph
        .edges
        .iter()
        .filter(|e| e.relation == Relation::Calls && e.target == n.id)
        .filter(|e| e.confidence != Confidence::Inferred)
        .filter_map(|e| graph.nodes.iter().find(|c| c.id == e.source))
        .filter(|c| c.id != n.id && is_test_node(graph, root, c, &test_path))
        .map(|c| {
            (
                c.path.as_str(),
                span_of(c).map_or(0, |s| s.0),
                c.name.as_str(),
            )
        })
        .collect();
    found.sort();
    found.dedup();
    found
        .into_iter()
        .map(|(p, l, name)| format!("test  {p}:{l}  {name}"))
        .collect()
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// The history rows from the output of `git log -L ... --format=%h%x09%ad%x09%s`.
/// The first line is the last commit. The last line is the introducing
/// commit.
fn history_from_log(log: &str) -> Vec<String> {
    let rows: Vec<String> = log
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let (sha, date, subject) = (p.next()?, p.next()?, p.next()?);
            Some(format!(
                "{} {}  commit subject (untrusted): {}",
                clean(sha, 12),
                clean(date, 12),
                clean(subject, LINE_MAX)
            ))
        })
        .collect();
    match (rows.first(), rows.last()) {
        (Some(last), Some(first)) if rows.len() > 1 => {
            vec![
                format!("history  introduced  {first}"),
                format!("history  last  {last}"),
            ]
        }
        (Some(only), _) => vec![format!("history  introduced and last  {only}")],
        _ => vec!["history skipped: git gave no commit".to_string()],
    }
}

/// The history rows of the span of `n`. A file with uncommitted changes, or
/// a span past the end of the committed file, gives a "history skipped" row
/// and no `log -L` call. `git` gives the stdout or a reason text.
fn history_rows(n: &Node, git: &GitFn) -> Vec<String> {
    let Some((a, b)) = span_of(n) else {
        return Vec::new();
    };
    let skip = |why: &str| vec![format!("history skipped: {why}")];
    match git(&["diff", "--quiet", "HEAD", "--", &n.path]) {
        Ok(_) => {}
        Err("git failed") if git(&["rev-parse", "--verify", "HEAD"]).is_ok() => {
            return skip("uncommitted changes");
        }
        Err(why) => return skip(why),
    }
    match git(&["show", &format!("HEAD:{}", n.path)]) {
        Ok(committed) if committed.lines().count() < b => return skip("the span is stale"),
        Ok(_) => {}
        Err(why) => return skip(why),
    }
    let range = format!("{a},{b}:{}", n.path);
    match git(&[
        "log",
        "-L",
        &range,
        "-s",
        "--format=%h%x09%ad%x09%s",
        "--date=short",
    ]) {
        Ok(log) => history_from_log(&log),
        Err(why) => skip(why),
    }
}

/// A git runner: arguments in, the stdout or a reason text out.
pub type GitFn<'a> = dyn Fn(&[&str]) -> Result<String, &'static str> + 'a;

/// The row lists of one symbol, in output order. Each row is one line.
pub struct SpanRows {
    /// The reason comments in the span.
    pub comments: Vec<String>,
    /// The test functions that call the symbol.
    pub tests: Vec<String>,
    /// The introducing commit and the last commit.
    pub history: Vec<String>,
}

/// Finds the rows of `n`. `git` is `None` in the hook path, so the hook runs
/// no git. A file node or a module node gives no row.
pub fn span_rows(
    root: &Path,
    graph: &Graph,
    n: &Node,
    git: Option<&GitFn>,
    test_path: impl Fn(&str) -> bool,
) -> SpanRows {
    if matches!(n.kind, Kind::File | Kind::Module) {
        return SpanRows {
            comments: Vec::new(),
            tests: Vec::new(),
            history: Vec::new(),
        };
    }
    SpanRows {
        comments: comment_rows(root, n),
        tests: test_rows(graph, root, n, test_path),
        history: git.map_or_else(Vec::new, |g| history_rows(n, g)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaks(secret_form: &str, secret: &str) {
        let out = redact(&format!("see {secret_form} end"));
        assert!(out.contains(MARK), "no mark for {secret_form}: {out}");
        assert!(!out.contains(secret), "leak of {secret} in {out}");
    }

    #[test]
    fn test_f6_redactor_hides_every_secret_shape() {
        let b64 = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5+/=";
        let hf = format!("hf_{}", "a1".repeat(15));
        let npm = format!("npm_{}", "b2".repeat(18));
        let cases: [(&str, &str); 25] = [
            ("ghp_abcdefghijklmnop1234", "abcdefghij"),
            ("gho_abcdefghijklmnop1234", "abcdefghij"),
            ("github_pat_11ABCDEFG0abcdefgh", "ABCDEFG0"),
            ("glpat-abcdefghij1234567890", "abcdefghij"),
            ("AKIAIOSFODNN7EXAMPLE", "IOSFODNN7"),
            ("AIzaSyA1234567890abcdefghij", "1234567890"),
            ("xoxb-1234567890-abcdefghij", "1234567890"),
            ("xoxp-1234567890-abcdefghij", "1234567890"),
            ("sk-abcdefghijklmnop1234", "abcdefghij"),
            ("sk-ant-api03-abcdefghijkl", "abcdefghij"),
            ("sk_live_abcdefghijklmnop", "abcdefghij"),
            ("rk_live_abcdefghijklmnop", "abcdefghij"),
            (npm.as_str(), "b2b2b2b2"),
            (hf.as_str(), "a1a1a1a1"),
            ("Authorization: Bearer abc.def.ghi", "abc.def"),
            ("Authorization: Basic dXNlcjpwYXNz", "dXNlcjpwYXNz"),
            ("AccountKey=abcd1234efgh==;Other=1", "abcd1234"),
            ("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345", "sig12345"),
            ("password = hunter2", "hunter2"),
            ("SECRET_KEY=hunter3", "hunter3"),
            ("secret_key = 'hunter4'", "hunter4"),
            ("\"password\": \"hunter5\"", "hunter5"),
            ("api_key: \"hunter6\"", "hunter6"),
            ("MY_AUTH_TOKEN=hunter7", "hunter7"),
            (b64, "QUJDREVG"),
        ];
        for (form, secret) in cases {
            leaks(form, secret);
        }
        let pem = "a -----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY----- b";
        let out = redact(pem);
        assert!(!out.contains("MIIabc") && out.contains(MARK), "{out}");
        let out = redact("clone https://user:pw@host.example/x.git now");
        assert_eq!(out, "clone https://host.example/x.git now");
    }

    #[test]
    fn test_f6_redactor_keeps_what_is_not_a_secret() {
        let long_name = "AVeryLongCamelCaseIdentifierNameThatKeepsGoingAndGoing";
        let long_path = "crates/some/very/long/directory/path/to/a/source/file/here.rs";
        let keep = [
            "NOTE: the cache is never stale because we hash 3b7ef6a1c2d4e5f60718293a4b5c6d7e8f901234",
            "pub token: String,",
            "fn f(token: &str) -> bool",
            "token == other",
            "npm_short and hf_short and sk-x",
            long_name,
            long_path,
        ];
        for k in keep {
            assert_eq!(redact(k), k, "changed: {k}");
        }
    }

    #[test]
    fn test_f6_redactor_strips_ansi_and_controls() {
        assert_eq!(redact("\u{1b}[31mred\u{1b}[0m a\u{7}b\tc"), "red ab c");
        assert_eq!(redact("x\u{1b}]0;title\u{7}y"), "xy");
    }

    #[test]
    fn test_f6_clean_cuts_before_and_after_the_redaction() {
        let secret = format!("ghp_{}", "a1".repeat(20));
        let out = clean(&format!("{secret} {}", "x".repeat(300)), COMMENT_MAX);
        assert!(!out.contains("ghp_"), "{out}");
        assert!(out.chars().count() <= COMMENT_MAX + 3);
        // A huge text is cut to 4 times the limit before the redactor runs.
        let huge = "y".repeat(100_000);
        assert!(clean(&huge, 10).chars().count() <= 13);
    }

    #[test]
    fn test_f6_a_reason_tag_is_a_whole_word_where_underscore_is_a_letter() {
        for yes in [
            " SAFETY: the pointer is valid",
            " WHY a lock",
            " TODO later",
            " it fails because of x",
            " so that the hook is fast",
            " never call this twice",
        ] {
            assert!(is_reason(yes), "{yes}");
        }
        for no in [
            " returns the count",
            " nevertheless fine",
            " NOTES are here",
            " NOTE_MAX is a limit",
            " the never_set flag",
        ] {
            assert!(!is_reason(no), "{no}");
        }
    }

    #[test]
    fn test_f6_history_shows_the_first_and_the_last_commit() {
        let log = "ccc3333\t2026-03-01\tthird\nbbb2222\t2026-02-01\tsecond\naaa1111\t2026-01-01\tfirst token=abc\n";
        let rows = history_from_log(log);
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].starts_with("history  introduced  aaa1111 2026-01-01"),
            "{rows:?}"
        );
        assert!(rows[0].contains("commit subject (untrusted): first token=[redacted]"));
        assert!(rows[1].starts_with("history  last  ccc3333"));
        assert_eq!(history_from_log("aaa1111\t2026-01-01\tx\n").len(), 1);
    }

    #[test]
    fn test_f6_history_failure_shows_the_reason() {
        let n = Node {
            id: "a.rs#f".into(),
            name: "f".into(),
            kind: Kind::Function,
            path: "a.rs".into(),
            span: "L1-L2".into(),
            signature: None,
            exported: false,
            origin: sieve_core::wiring::Origin::Ast,
            body_hash: String::new(),
            chars: None,
            body_text: None,
            summary_state: sieve_core::wiring::SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
            arity: None,
            variadic: None,
        };
        let rows = history_rows(&n, &|_| Err("timeout"));
        assert_eq!(rows, vec!["history skipped: timeout".to_string()]);
    }
}
