//! A small hand scanner for shell scripts. It mints one function node for
//! each shell function definition, with no grammar and no call edges.

use std::collections::HashSet;

use sieve_core::{Kind, Node, Origin, SummaryState};

use crate::build::hex_sha256;
use crate::extract::mint_id;

/// One scanned definition: name, and the 0-based first and last line.
struct Def {
    name: String,
    first: usize,
    last: usize,
}

/// The heredoc word that a `<<WORD` at `chars[i..]` opens, with the index
/// after it and whether leading tabs may precede the end line (`<<-`).
fn heredoc_word(chars: &[char], i: usize) -> Option<(String, usize, bool)> {
    let mut j = i + 2;
    let dash = chars.get(j) == Some(&'-');
    if dash {
        j += 1;
    }
    while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
        j += 1;
    }
    let q = chars.get(j).copied().filter(|c| *c == '\'' || *c == '"');
    j += usize::from(q.is_some());
    let from = j;
    while chars
        .get(j)
        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
    {
        j += 1;
    }
    let word: String = chars[from..j].iter().collect();
    if word.is_empty() || word.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    j += usize::from(q.is_some() && chars.get(j) == q.as_ref());
    Some((word, j, dash))
}

/// Brace events of the whole file as `(line, +1 or -1)`, and the lines that
/// start inside a quote, a `$(...)`, a backtick span or a heredoc body.
/// Braces in those, and in comments, do not count.
fn scan_braces(source: &str) -> (Vec<(usize, i32)>, HashSet<usize>) {
    let mut events = Vec::new();
    let mut inside = HashSet::new();
    let (mut quote, mut cmd, mut cmd_quote) = (None::<char>, 0usize, None::<char>);
    let mut tick = false;
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut lines = source.split('\n').enumerate();
    while let Some((n, line)) = lines.next() {
        let open = quote.is_some() || cmd > 0 || tick;
        // A function head at column 0 ends a runaway quote.
        if open && !line.starts_with(char::is_whitespace) && def_name(line, None).is_some() {
            (quote, cmd, cmd_quote, tick) = (None, 0, None, false);
        } else if open {
            inside.insert(n);
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if tick {
                if c == '\\' {
                    i += 1;
                } else if c == '`' {
                    tick = false;
                }
            } else if let Some(q) = quote {
                if c == '\\' && q == '"' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            } else if cmd > 0 {
                if let Some(q) = cmd_quote {
                    if c == '\\' && q == '"' {
                        i += 1;
                    } else if c == q {
                        cmd_quote = None;
                    }
                } else {
                    match c {
                        '\\' => i += 1,
                        '\'' | '"' => cmd_quote = Some(c),
                        '`' => tick = true,
                        '(' => cmd += 1,
                        ')' => cmd -= 1,
                        _ => {}
                    }
                }
            } else {
                match c {
                    '\\' => i += 1,
                    '\'' | '"' => quote = Some(c),
                    '`' => tick = true,
                    '#' if i == 0 || chars[i - 1].is_whitespace() => break,
                    '$' if chars.get(i + 1) == Some(&'(') => {
                        cmd = 1;
                        i += 1;
                    }
                    '<' if chars.get(i + 1) == Some(&'<') && chars.get(i + 2) != Some(&'<') => {
                        if let Some((word, next, dash)) = heredoc_word(&chars, i) {
                            pending.push((word, dash));
                            i = next - 1;
                        } else {
                            i += 1;
                        }
                    }
                    '{' => events.push((n, 1)),
                    '}' => events.push((n, -1)),
                    _ => {}
                }
            }
            i += 1;
        }
        for (word, dash) in pending.drain(..) {
            for (m, body) in lines.by_ref() {
                inside.insert(m);
                let end = if dash {
                    body.trim_start_matches('\t')
                } else {
                    body
                };
                if end == word {
                    break;
                }
            }
        }
    }
    (events, inside)
}

/// The function name a line defines, or `None`. The forms are `name() {`,
/// `name () {`, `function name {` and `function name() {`. The `{` may start
/// the `next` line.
fn def_name(line: &str, next: Option<&str>) -> Option<String> {
    let t = line.trim_start();
    let (t, has_kw) = match t.strip_prefix("function") {
        Some(r) if r.starts_with(char::is_whitespace) => (r.trim_start(), true),
        _ => (t, false),
    };
    let end = t
        .find(|c: char| !(c.is_alphanumeric() || "_.:-".contains(c)))
        .unwrap_or(t.len());
    let (name, rest) = t.split_at(end);
    if name.is_empty() {
        return None;
    }
    let rest = rest.trim_start();
    let (rest, has_parens) = match rest.strip_prefix('(') {
        Some(r) => (r.trim_start().strip_prefix(')')?.trim_start(), true),
        None => (rest, false),
    };
    let brace_next = next.is_some_and(|l| l.trim_start().starts_with('{'));
    let ok_rest = rest.starts_with('{') || (rest.is_empty() && (has_parens || brace_next));
    (ok_rest && (has_kw || has_parens)).then(|| name.to_string())
}

/// Finds every function definition with its line span.
fn find_defs(source: &str) -> Vec<Def> {
    let (events, inside) = scan_braces(source);
    let lines: Vec<&str> = source.split('\n').collect();
    let heads: Vec<(usize, String)> = lines
        .iter()
        .enumerate()
        .filter(|(n, _)| !inside.contains(n))
        .filter_map(|(n, l)| def_name(l, lines.get(n + 1).copied()).map(|name| (n, name)))
        .collect();
    let last_line = lines.len().saturating_sub(1);
    let mut defs = Vec::new();
    for (k, (first, name)) in heads.iter().enumerate() {
        // ponytail: an unsure span ends before the next definition or at the file end.
        let fallback = heads.get(k + 1).map_or(last_line, |(next, _)| next - 1);
        let start = events.partition_point(|(l, _)| l < first);
        // A column 0 head ends the scan, so an unbalanced file stays linear.
        let stop = heads[k + 1..]
            .iter()
            .find(|(n, _)| !lines[*n].starts_with(char::is_whitespace))
            .map_or(usize::MAX, |(n, _)| *n);
        let mut depth = 0i32;
        let mut last = None;
        if events
            .get(start)
            .is_some_and(|(l, d)| *d == 1 && *l <= first + 1)
        {
            for (l, d) in events[start..].iter().take_while(|(l, _)| *l < stop) {
                depth += d;
                if depth <= 0 {
                    last = (depth == 0).then_some(*l);
                    break;
                }
            }
        }
        let last = last.filter(|l| *l <= last_line).unwrap_or(fallback);
        defs.push(Def {
            name: name.clone(),
            first: *first,
            last: last.max(*first),
        });
    }
    defs
}

/// Mints one function node for each shell function definition in `source`.
pub fn extract_shell(path: &str, source: &str) -> Vec<Node> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut minted = HashSet::new();
    find_defs(source)
        .into_iter()
        .map(|d| {
            let body = lines[d.first..=d.last].join("\n");
            let sig = lines[d.first].trim().trim_end_matches('{').trim_end();
            Node {
                id: mint_id(format!("{path}#{}", d.name), &mut minted),
                name: d.name,
                kind: Kind::Function,
                owner: None,
                path: path.to_string(),
                span: sieve_core::span(d.first as u32 + 1, d.last as u32 + 1),
                signature: (!sig.is_empty()).then(|| sig.to_string()),
                exported: true,
                origin: Origin::Generic,
                body_hash: hex_sha256(body.as_bytes()),
                chars: None,
                body_text: Some(body.split_whitespace().collect::<Vec<_>>().join(" ")),
                arity: None,
                variadic: None,
                summary_state: SummaryState::Pending,
                summary: None,
                crux: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(src: &str) -> Vec<(String, String)> {
        extract_shell("a.sh", src)
            .into_iter()
            .map(|n| (n.name, n.span))
            .collect()
    }

    #[test]
    fn shell_finds_every_definition_form() {
        let src = "a() {\n  :\n}\nb () {\n  :\n}\nfunction c {\n  :\n}\n  function d() {\n  :\n}\n";
        let names: Vec<String> = spans(src).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["a", "b", "c", "d"]);
    }

    #[test]
    fn shell_span_handles_nested_braces_and_quoted_braces() {
        let src = "f() {\n  if x; then\n    echo \"}\" # }\n    v=${A}\n    w=$(echo })\n  fi\n  { :; }\n}\necho done\ng() { :; }\n";
        let s = spans(src);
        assert_eq!(s[0].0, "f");
        assert_eq!(s[0].1, sieve_core::span(1, 8));
        assert_eq!(s[1].1, sieve_core::span(10, 10));
    }

    #[test]
    fn shell_echo_of_a_definition_gives_no_node() {
        assert!(spans("echo \"f() {\"\n").is_empty());
        assert!(spans("x=\"\n  f() {\n}\"\n").is_empty());
    }

    #[test]
    fn shell_unsure_span_ends_before_the_next_definition() {
        assert_eq!(
            spans("a() {\n:\nb() {\n:\n}\n")[0].1,
            sieve_core::span(1, 2)
        );
    }

    #[test]
    fn shell_heredoc_with_an_apostrophe_keeps_later_functions() {
        let src = "a() {\n  cat <<EOF\ndon't {\nEOF\n}\nb() {\n  cat <<-'X'\n\tit's\n\tX\n}\n";
        let s = spans(src);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].1, sieve_core::span(1, 5));
        assert_eq!(s[1].1, sieve_core::span(6, 10));
    }

    #[test]
    fn shell_quote_inside_command_substitution_and_backticks() {
        let src = "f() {\n  x=$(echo \")\")\n  y=`echo '}'`\n}\ng() { :; }\n";
        let s = spans(src);
        assert_eq!(s[0].1, sieve_core::span(1, 4));
        assert_eq!(s[1].1, sieve_core::span(5, 5));
    }

    #[test]
    fn shell_unclosed_brace_still_finds_every_function() {
        let names: Vec<String> = spans("a() {\n:\nb() {\n:\n}\nc() {\n:\n}\n")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn shell_function_keyword_with_the_brace_on_the_next_line() {
        let s = spans("function f\n{\n  :\n}\n");
        assert_eq!(s[0].0, "f");
        assert_eq!(s[0].1, sieve_core::span(1, 4));
    }

    #[test]
    fn shell_never_panics_on_odd_input() {
        for src in [
            "",
            "{",
            "}",
            "f()",
            "function",
            "f() {",
            "\"",
            "$(",
            "}}}\nf() {\n",
        ] {
            let _ = spans(src);
        }
    }
}
