//! Renders an [`AskResult`] as the plain-text `ask` report
//! (`ask-ranking.md` section 7, `formatAsk`), and as pretty JSON
//! (section 7.10).

use std::sync::OnceLock;

use regex::Regex;
use sieve_core::collate::collate;

use super::{scope_label, AskHit, AskMode, AskResult};

/// The word a mode serializes to in the header and in JSON.
fn mode_word(mode: AskMode) -> &'static str {
    match mode {
        AskMode::Lexical => "lexical",
        AskMode::Empty => "empty",
        AskMode::Structural => "structural",
    }
}

/// Swaps the words the text report does not use for plain ones. The JSON
/// report keeps the note as it is.
fn plain_words(line: &str) -> String {
    line.replace("no matching nodes", "no matching symbols")
        .replace("outgoing edges from", "outgoing links from")
        .replace("precise edges", "exact links")
        .replace("workspace repo(s)", "workspace repos")
}

/// The note block: every line starting `structural index:` gets a `⚠ `
/// marker; a benign structural note prints plain (section 6.4).
fn note_block(note: &Option<String>) -> String {
    match note {
        Some(note) => note
            .split('\n')
            .map(plain_words)
            .map(|line| {
                if line.starts_with("structural index:") {
                    format!("⚠ {line}")
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

/// The escalation nudge appended after a thin (`<= 3` hits) lexical or
/// empty result (section 7.7).
fn escalation_nudge(r: &AskResult) -> String {
    if (r.mode != AskMode::Lexical && r.mode != AskMode::Empty) || r.hits.len() > 3 {
        return String::new();
    }
    let n = r.hits.len();
    let head = if n == 0 {
        "no hits".to_string()
    } else {
        format!("only {n} hit{}", if n == 1 { "" } else { "s" })
    };
    let product = sieve_core::product();
    format!(
        "\n\n{} {head} — don't re-ask with new wording; switch tool: \
        `{} grep \"<literal>\"` for every occurrence · `{} skeleton <file>` for a file's \
        full API · `{} callers <symbol>` for who-uses.",
        product.prefix(),
        product.name,
        product.name,
        product.name
    )
}

/// The multi-scope footer: `matched in:` over the displayed hits' scopes,
/// then one `also matched:` line per gated-out scope (section 7.6,
/// `ask.ts`'s `scopeFooterLines`: counts come from the rendered hits, not
/// from the fused scope list, so a top-lock reorder still counts right).
fn scope_footer_lines(r: &AskResult) -> Vec<String> {
    let Some(scopes) = &r.scopes else {
        return Vec::new();
    };
    // Insertion-ordered scope counts (rule book 9.1): a `Vec` plus a linear
    // scan, since a hit list is a handful of scopes at most.
    let mut per_scope: Vec<(String, usize)> = Vec::new();
    for h in &r.hits {
        let Some(scope) = &h.scope else { continue };
        match per_scope.iter_mut().find(|(s, _)| s == scope) {
            Some((_, count)) => *count += 1,
            None => per_scope.push((scope.clone(), 1)),
        }
    }
    per_scope.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| collate(&a.0, &b.0)));

    let mut out = Vec::new();
    if !per_scope.is_empty() {
        let parts: Vec<String> = per_scope
            .iter()
            .map(|(scope, count)| format!("{} ({count})", scope_label(scope)))
            .collect();
        out.push(format!("matched in: {}", parts.join(" · ")));
    }
    for m in &scopes.also_matched {
        let label = scope_label(&m.scope);
        out.push(format!("also matched: {label} — narrow with --in {label}"));
    }
    out
}

/// The row of one hit: `name  kind  path:start-end`. A concept hit shows
/// its sources in place of a path.
fn row_of(h: &AskHit) -> String {
    let (name, kind) = sieve_core::voice::split_title(&h.title);
    let pointer = sieve_core::voice::pointer(&h.pointer);
    // A structural hit has no kind in its title: it shows `caller` or
    // `callee`, from the hit kind.
    let kind = sieve_core::voice::short_kind(kind.unwrap_or(&h.kind));
    format!("{name}  {kind}  {pointer}")
}

/// Renders an [`AskResult`] as the plain-text `ask` report.
pub fn format_ask(r: &AskResult) -> String {
    let count = r.hits.len();
    let mode = match r.mode {
        AskMode::Lexical => String::new(),
        other => format!(" \u{b7} {}", mode_word(other)),
    };
    let head = format!(
        "ask  {} \u{b7} {count} {}{mode}",
        r.query,
        if count == 1 { "hit" } else { "hits" },
    );
    let note_block = note_block(&r.note);

    if r.hits.is_empty() {
        let body = if note_block.is_empty() {
            "no matches."
        } else {
            note_block.as_str()
        };
        return format!("{head}\n\n{body}{}\n", escalation_nudge(r));
    }

    let mut lines: Vec<String> = if note_block.is_empty() {
        vec![head, String::new()]
    } else {
        vec![head, String::new(), note_block, String::new()]
    };

    if r.mode == AskMode::Structural {
        for h in &r.hits {
            let relation = h.relation.as_deref().unwrap_or("");
            let tail = if h.snippet.is_empty() {
                String::new()
            } else {
                format!(" — {}", h.snippet)
            };
            lines.push(format!("{}  \u{b7} {relation}{tail}", row_of(h)));
            if let Some(code) = &h.code {
                lines.push(String::new());
                lines.push("```".to_string());
                lines.push(code.clone());
                lines.push("```".to_string());
                lines.push(String::new());
            }
        }
    } else {
        for (i, h) in r.hits.iter().enumerate() {
            let label = match (&r.scopes, &h.scope) {
                (Some(_), Some(scope)) if !scope.is_empty() => format!("[{scope}/] "),
                _ => String::new(),
            };
            lines.push(format!("{}  {label}{}", i + 1, row_of(h)));
            if !h.snippet.is_empty() {
                lines.push(format!("   {}", h.snippet));
            }
            if !h.related.is_empty() {
                lines.push(format!("   related: {}", h.related.join(", ")));
            }
            if let Some(code) = &h.code {
                lines.push(String::new());
                lines.push("```".to_string());
                lines.push(code.clone());
                lines.push("```".to_string());
            }
            lines.push(String::new());
        }
        lines.extend(scope_footer_lines(r));
    }

    let body = lines.join("\n").trim_end().to_string();
    format!("{body}{}\n", escalation_nudge(r))
}

/// Matches a whole-number float field, so it can be rewritten from
/// `serde_json`'s `1.0` to JavaScript's `1`.
fn integer_float_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r": (-?\d+)\.0([,\n}])").ok())
        .as_ref()
}

/// Renders an [`AskResult`] as `JSON.stringify(r, null, 2) + "\n"` would
/// (section 7.10). `serde_json` prints a whole-number float as `1.0`;
/// JavaScript prints `1`. Rewritten here the same way
/// `askindex::write_ask_index` rewrites `avgBodyLen`.
pub fn format_ask_json(r: &AskResult) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string_pretty(r)?;
    let fixed = match integer_float_re() {
        Some(re) => re.replace_all(&json, ": $1$2").into_owned(),
        None => json,
    };
    Ok(format!("{fixed}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::AskHit;

    fn empty_result(mode: AskMode, hits: Vec<AskHit>, note: Option<String>) -> AskResult {
        AskResult {
            query: "q".to_string(),
            mode,
            subject: None,
            hits,
            scopes: None,
            coverage: None,
            coverage_strong: None,
            ranking: None,
            note,
            saved: None,
            pagerank_for_test: None,
        }
    }

    fn hit(title: &str) -> AskHit {
        AskHit {
            id: title.to_string(),
            kind: "symbol".to_string(),
            title: title.to_string(),
            pointer: format!("a.ts:L1-L{}", title.len()),
            snippet: String::new(),
            related: Vec::new(),
            relation: None,
            score: 1.0,
            scope: None,
            code: None,
            scope_last: false,
            path: "a.ts".to_string(),
            span: Some("L1-L1".to_string()),
        }
    }

    /// P1-06 (DV1): a structural result prints `subject` after `mode` and
    /// no `coverage`; `saved` prints last when set (DV5, P1-02).
    #[test]
    fn test_p1_06_json_prints_subject_and_drops_a_none_coverage() {
        let mut r = empty_result(AskMode::Structural, vec![hit("one")], Some("n".to_string()));
        r.subject = Some("one".to_string());
        r.coverage = None;
        r.coverage_strong = None;
        r.saved = Some(crate::ask::AskSaved {
            files: 1,
            baseline_chars: 76,
        });
        let json = format_ask_json(&r).expect("serializes");
        assert!(json.starts_with("{\n  \"query\": \"q\",\n  \"mode\": \"structural\",\n  \"subject\": \"one\",\n  \"hits\": ["));
        assert!(!json.contains("coverage"));
        assert!(json.ends_with("  \"note\": \"n\",\n  \"saved\": {\n    \"files\": 1,\n    \"baselineChars\": 76\n  }\n}\n"));
    }

    #[test]
    fn the_text_report_uses_plain_words_and_the_row_grammar() {
        let r = empty_result(
            AskMode::Empty,
            vec![],
            Some("no matching nodes".to_string()),
        );
        assert!(format_ask(&r).contains("no matching symbols"));

        let mut h = hit("run \u{b7} function");
        h.pointer = "a.ts:L4-L9".to_string();
        let r = empty_result(AskMode::Lexical, vec![h; 4], None);
        let text = format_ask(&r);
        assert!(
            text.starts_with("ask  q \u{b7} 4 hits\n\n1  run  fn  a.ts:4-9\n"),
            "{text}"
        );
    }

    #[test]
    fn the_nudge_is_singular_at_exactly_one_hit() {
        let r = empty_result(AskMode::Lexical, vec![hit("one")], None);
        assert!(format_ask(&r).contains("only 1 hit —"));
    }

    #[test]
    fn the_nudge_is_plural_at_more_than_one_hit_and_silent_above_three() {
        let r = empty_result(AskMode::Lexical, vec![hit("a"), hit("b")], None);
        assert!(format_ask(&r).contains("only 2 hits —"));

        let r = empty_result(
            AskMode::Lexical,
            vec![hit("a"), hit("b"), hit("c"), hit("d")],
            None,
        );
        assert!(!format_ask(&r).contains("[sieve]"));
    }

    #[test]
    fn the_nudge_reads_no_hits_on_an_empty_result() {
        let r = empty_result(AskMode::Empty, vec![], None);
        assert!(format_ask(&r).contains("[sieve] no hits —"));
    }

    #[test]
    fn a_fallthrough_note_gets_the_warning_marker_but_a_benign_note_does_not() {
        let r = empty_result(
            AskMode::Lexical,
            vec![hit("a")],
            Some("structural index: no entries for 'x' — showing lexical matches".to_string()),
        );
        assert!(format_ask(&r).contains("⚠ structural index:"));

        let r = empty_result(
            AskMode::Structural,
            vec![hit("a")],
            Some("callers / references of run".to_string()),
        );
        assert!(!format_ask(&r).contains('⚠'));
        assert!(format_ask(&r).contains("callers / references of run"));
    }

    /// The render prints `related:` right after the snippet, in
    /// frontmatter order, for a concept hit only (note section 2.8).
    #[test]
    fn a_concept_hits_related_line_prints_after_the_snippet_in_frontmatter_order() {
        let mut concept = hit("Widget");
        concept.kind = "concept".to_string();
        concept.snippet = "the widget summary".to_string();
        concept.related = vec!["zeta".to_string(), "alpha".to_string()];
        let r = empty_result(AskMode::Lexical, vec![concept], None);
        let rendered = format_ask(&r);
        let snippet_line = rendered
            .lines()
            .position(|l| l.trim() == "the widget summary");
        let related_line = rendered
            .lines()
            .position(|l| l.trim() == "related: zeta, alpha");
        assert!(snippet_line.is_some() && related_line.is_some());
        assert!(related_line.unwrap() == snippet_line.unwrap() + 1);
    }

    #[test]
    fn a_symbol_hits_render_prints_no_related_line() {
        let r = empty_result(AskMode::Lexical, vec![hit("a")], None);
        assert!(!format_ask(&r).contains("related:"));
    }

    #[test]
    fn format_ask_json_prints_a_whole_number_score_with_no_decimal() {
        let r = empty_result(AskMode::Lexical, vec![hit("a")], None);
        let json = format_ask_json(&r).expect("json renders");
        assert!(json.contains("\"score\": 1\n") || json.contains("\"score\": 1,\n"));
        assert!(!json.contains("1.0"));
    }
}
