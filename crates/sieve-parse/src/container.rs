//! The container tier: a file that is not itself a language but a
//! wrapper around one. `.vue`, `.svelte` and `.astro` are the rows. See
//! the `languages-lsp.md` note section 2.
//!
//! A WASM Vue grammar would only find where a `<script>`
//! block starts and ends; the block itself goes to the depth-tier
//! TypeScript extractor. No Vue grammar crate loads at the `tree-sitter`
//! 0.23 ABI this workspace pins, so Sieve finds that same block by a
//! byte scan over the source instead (`languages-lsp.md` line 126).

use std::collections::{HashMap, HashSet};

use sieve_core::{span, Kind, Node, Origin, SummaryState};

use crate::build::hex_sha256;
use crate::extract::{file_residual, mint_id, Extractor, RawEdge};

/// One container language: its name and the file extensions it claims.
pub struct ContainerLang {
    pub name: &'static str,
    pub exts: &'static [&'static str],
}

/// The container registry. A wrong `body` node type would silently misplace
/// every span, so a new row is added only once a real fixture verifies it. The
/// Svelte and Astro fixtures are in the tests below.
pub const CONTAINER_LANGS: &[ContainerLang] = &[
    ContainerLang {
        name: "vue",
        exts: &[".vue"],
    },
    ContainerLang {
        name: "svelte",
        exts: &[".svelte"],
    },
    ContainerLang {
        name: "astro",
        exts: &[".astro"],
    },
];

/// Finds the container language with an extension of `path`.
fn lang_by_ext(path: &str) -> Option<&'static ContainerLang> {
    let lower = path.to_lowercase();
    CONTAINER_LANGS
        .iter()
        .find(|l| l.exts.iter().any(|ext| lower.ends_with(ext)))
}

/// Finds the container language that claims `path`, or `None`.
pub fn container_lang_of(path: &str) -> Option<&'static ContainerLang> {
    lang_by_ext(path)
}

/// One embedded `<script>` block: its raw text, and the 0-indexed row its
/// text starts on, for shifting the inner extractor's spans back onto the
/// `.vue` file.
pub(crate) struct Block {
    pub(crate) text: String,
    pub(crate) start_row: u32,
}

/// Finds the script blocks of a container file by language name:
/// `vue`, `svelte` or `astro`.
pub(crate) fn scan_blocks_for(name: &str, source: &str) -> Vec<Block> {
    match name {
        "svelte" => scan_markup_blocks(source, 0),
        "astro" => {
            let (front, body_from) = astro_frontmatter(source);
            let mut out: Vec<Block> = front.into_iter().collect();
            out.extend(scan_markup_blocks(source, body_from));
            out
        }
        _ => scan_blocks(source),
    }
}

/// Finds the Astro frontmatter: the text between the `---` line at the
/// file start and the next `---` line. Gives the block and the byte where
/// the markup starts. With no closed fence, gives no block and offset 0.
fn astro_frontmatter(source: &str) -> (Option<Block>, usize) {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let bom = source.len() - text.len();
    let Some(first) = text.split_inclusive('\n').next() else {
        return (None, 0);
    };
    if first.trim_end() != "---" {
        return (None, 0);
    }
    let body_start = bom + first.len();
    let mut pos = body_start;
    for line in source[body_start..].split_inclusive('\n') {
        if line.trim_end() == "---" {
            let block = (pos > body_start).then(|| Block {
                text: source[body_start..pos].to_string(),
                start_row: 1,
            });
            return (block, pos + line.len());
        }
        pos += line.len();
    }
    (None, 0)
}

/// Finds the end of a `{...}` expression that opens at `open`. Gives the
/// byte after the closing brace, or `None` if no brace closes it. The
/// caller then goes on after the stray `{`, so it hides no later script.
/// A brace inside a quoted or backtick string does not count.
///
/// ponytail: the earliest of the two counts wins. It is never later than the
/// old naive count, and a wrong early end only shows extra markup text, while
/// a late end would swallow a script. A `${...}` in a backtick string is
/// skipped as text. Use a real expression parse if a fixture needs more.
fn skip_expr(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    match (brace_end(bytes, open, false), brace_end(bytes, open, true)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Finds the byte after the `}` that closes the `{` at `open`. With
/// `track_strings`, a brace inside a string does not count.
fn brace_end(bytes: &[u8], open: usize, track_strings: bool) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match (quote, b) {
            (Some(_), _) if escaped => escaped = false,
            (Some(_), b'\\') => escaped = true,
            (Some(q), _) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'' | b'`') if track_strings => quote = Some(b),
            (None, b'{') => depth += 1,
            (None, b'}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Finds the `>` that ends the opening tag at `open`, and skips a `>` in a
/// quoted attribute value (`generics="T extends Array<string>"`, `=>`).
fn tag_end(source: &str, open: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, b) in source.bytes().enumerate().skip(open) {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// Finds every top-level `<script>` block in markup from byte `from`, in
/// document order, for Svelte and Astro. The scan skips `<style>` bodies,
/// `{...}` expressions (so a `<script>` in a string there is text) and a
/// `<script>` inside a Svelte `{#...}` block (not a top-level block).
fn scan_markup_blocks(source: &str, from: usize) -> Vec<Block> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut blocks_open = 0i32;
    let mut i = from;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<!--") {
            // An HTML comment holds text only: skip it before any other rule.
            i = source[i + 4..]
                .find("-->")
                .map_or(bytes.len(), |p| i + 4 + p + 3);
            continue;
        }
        match bytes[i] {
            b'{' => {
                let Some(end) = skip_expr(source, i) else {
                    i += 1; // a stray `{`: go on after it
                    continue;
                };
                match bytes.get(i + 1) {
                    Some(b'#') => blocks_open += 1,
                    Some(b'/') => blocks_open -= 1,
                    _ => {}
                }
                i = end;
            }
            b'<' => {
                let (tag, close) = if tag_starts_at(source, i, "<script") {
                    ("<script", "</script>")
                } else if tag_starts_at(source, i, "<style") {
                    ("<style", "</style>")
                } else {
                    i += 1;
                    continue;
                };
                let Some(gt) = tag_end(source, i) else {
                    break; // an unterminated opening tag ends the scan
                };
                let body_start = gt + 1;
                let Some(rel_close) = source[body_start..].find(close) else {
                    i = body_start;
                    continue;
                };
                let body_end = body_start + rel_close;
                if tag == "<script" && blocks_open <= 0 && body_end > body_start {
                    out.push(Block {
                        text: source[body_start..body_end].to_string(),
                        start_row: source[..body_start].matches('\n').count() as u32,
                    });
                }
                i = body_end + close.len();
            }
            _ => i += 1,
        }
    }
    out
}

/// True if `tag` starts at `idx` and its name ends there.
fn tag_starts_at(source: &str, idx: usize, tag: &str) -> bool {
    source[idx..].starts_with(tag) && tag_name_ends_at(source, idx, tag)
}

/// True if the byte right after `tag` at `idx` in `source` ends the tag
/// name rather than continuing it, so `<script` matches `<script>` and
/// `<script lang="ts">` but not `<script-editor>` (a Vue grammar draws
/// this same line by node type, not text; see `scan_blocks` below).
fn tag_name_ends_at(source: &str, idx: usize, tag: &str) -> bool {
    match source.as_bytes().get(idx + tag.len()) {
        None => true,
        Some(b) => matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/'),
    }
}

/// Finds the next `tag`-named opening tag at or after `from`, skipping any
/// text match whose next byte does not end the tag name.
fn find_tag(source: &str, from: usize, tag: &str) -> Option<usize> {
    let mut search_from = from;
    loop {
        let rel = source[search_from..].find(tag)?;
        let idx = search_from + rel;
        if tag_name_ends_at(source, idx, tag) {
            return Some(idx);
        }
        search_from = idx + tag.len();
    }
}

/// Every `<template>...</template>` byte range, so `scan_blocks` can skip
/// a `<script` that only appears as plain text inside a template (a Vue
/// grammar never treats that text as a `script_element` in the first
/// place, because it is nested under `template_element` instead of being
/// a direct child of the document root).
///
/// ponytail: this assumes one `<template>` does not nest inside another,
/// which holds for every real `.vue` file; a full HTML parse would drop
/// that assumption but is not needed here.
fn template_ranges(source: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(tag_start) = find_tag(source, from, "<template") {
        let Some(rel_gt) = source[tag_start..].find('>') else {
            break; // an unterminated opening tag ends the scan
        };
        let body_start = tag_start + rel_gt + 1;
        let Some(rel_close) = source[body_start..].find("</template>") else {
            from = body_start;
            continue; // an unterminated block: keep scanning after it
        };
        let end = body_start + rel_close + "</template>".len();
        out.push((tag_start, end));
        from = end;
    }
    out
}

/// Finds every direct `<script ...>` ... `</script>` block, in document
/// order. `raw_text`'s offset is the byte right after the opening tag's
/// `>`; an empty `<script></script>` yields an empty block, which the
/// caller skips (`languages-lsp.md` line 118). A `<script` inside a
/// `<template>...</template>` range is skipped, matching a Vue grammar:
/// that text is nested under `template_element` there, never a
/// `script_element` at the document root.
fn scan_blocks(source: &str) -> Vec<Block> {
    let templates = template_ranges(source);
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(tag_start) = find_tag(source, from, "<script") {
        if let Some(&(_, end)) = templates
            .iter()
            .find(|&&(s, e)| tag_start >= s && tag_start < e)
        {
            from = end;
            continue; // a literal "<script" inside a <template>, not a real tag
        }
        let Some(rel_gt) = source[tag_start..].find('>') else {
            break; // an unterminated opening tag ends the scan
        };
        let body_start = tag_start + rel_gt + 1;
        let Some(rel_close) = source[body_start..].find("</script>") else {
            from = body_start;
            continue; // an unterminated block: keep scanning after it
        };
        let body_end = body_start + rel_close;
        if body_end > body_start {
            let start_row = source[..body_start].matches('\n').count() as u32;
            out.push(Block {
                text: source[body_start..body_end].to_string(),
                start_row,
            });
        }
        from = body_end + "</script>".len();
    }
    out
}

/// Shifts a `L<start>-L<end>` span down by `lines`. A span in any other
/// shape is returned untouched (`languages-lsp.md` line 120).
fn shift_span(span_text: &str, lines: u32) -> String {
    let Some(rest) = span_text.strip_prefix('L') else {
        return span_text.to_string();
    };
    let Some((start_str, end_str)) = rest.split_once("-L") else {
        return span_text.to_string();
    };
    let (Ok(start), Ok(end)) = (start_str.parse::<u32>(), end_str.parse::<u32>()) else {
        return span_text.to_string();
    };
    span(start + lines, end + lines)
}

/// Extracts one container file (`.vue`, `.svelte` or `.astro`): its own file
/// node, every symbol from its `<script>` block(s) run through the depth-tier
/// TypeScript extractor with shifted spans, and every raw edge from those
/// blocks with ids rewritten to match. Never fails: a block the inner extractor
/// cannot parse degrades to fewer nodes. (`languages-lsp.md` note).
pub fn extract_container(
    rel: &str,
    source: &str,
    extractor: &mut Extractor,
) -> (Node, Vec<Node>, Vec<RawEdge>) {
    let mut nodes = Vec::new();
    let mut raw_edges = Vec::new();
    let mut residuals: Vec<String> = Vec::new();
    let mut minted: HashSet<String> = HashSet::new();
    minted.insert(rel.to_string());

    let name = lang_by_ext(rel).map_or("vue", |l| l.name);
    for block in scan_blocks_for(name, source) {
        let (symbols, edges) = extractor.extract_file(rel, &block.text, "typescript");
        residuals.push(file_residual(&block.text, &symbols));

        let mut renamed: HashMap<String, String> = HashMap::new();
        for mut node in symbols {
            let id = mint_id(node.id.clone(), &mut minted);
            if id != node.id {
                renamed.insert(node.id.clone(), id.clone());
            }
            node.id = id;
            node.span = shift_span(&node.span, block.start_row);
            nodes.push(node);
        }
        for mut edge in edges {
            if let Some(new_source) = renamed.get(&edge.source) {
                edge.source = new_source.clone();
            }
            if let Some(target) = &edge.target_id {
                if let Some(new_target) = renamed.get(target) {
                    edge.target_id = Some(new_target.clone());
                }
            }
            raw_edges.push(edge);
        }
    }

    let end_line = source.matches('\n').count() as u32 + 1;
    let file_node = Node {
        id: rel.to_string(),
        name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
        kind: Kind::File,
        owner: None,
        path: rel.to_string(),
        span: span(1, end_line.max(1)),
        signature: None,
        exported: true,
        // "ast", not "generic": the symbols under this file come from the
        // depth-tier extractor with real bindings and specifiers.
        // `resolve.rs` gates the guess-by-name fallback on
        // `origin == generic`, and this file must not take it.
        origin: Origin::Ast,
        body_hash: hex_sha256(source.as_bytes()),
        chars: Some(source.encode_utf16().count() as u64),
        body_text: Some(residuals.join("\n")),
        arity: None,
        variadic: None,
        summary_state: SummaryState::Pending,
        summary: None,
        crux: None,
    };
    (file_node, nodes, raw_edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVELTE: &str = "<script lang=\"ts\">\n  let n = $state(0);\n  export function bump(): number {\n    n += 1;\n    return n;\n  }\n</script>\n\n<script context=\"module\">\n  export function shared() {\n    return 2;\n  }\n</script>\n\n{#if n > 0}\n  <script>function ghost() {}</script>\n{/if}\n{\"<script>function str() {}</script>\"}\n<style>\n  .a { color: red; }\n  function css() {}\n</style>\n<p on:click={bump}>{n}</p>\n";

    fn names(nodes: &[Node]) -> Vec<(String, String)> {
        nodes
            .iter()
            .map(|n| (n.name.clone(), n.span.clone()))
            .collect()
    }

    #[test]
    fn test_svelte_ts_and_module_scripts_give_nodes_on_real_lines() {
        let mut ex = Extractor::new().expect("build extractor");
        let (file, nodes, _) = extract_container("A.svelte", SVELTE, &mut ex);
        let got = names(&nodes);
        assert!(got.contains(&("bump".into(), "L3-L6".into())), "{got:?}");
        assert!(
            got.contains(&("shared".into(), "L10-L12".into())),
            "{got:?}"
        );
        assert_eq!(file.body_hash, hex_sha256(SVELTE.as_bytes()));
    }

    #[test]
    fn test_svelte_markup_style_if_block_and_string_give_no_nodes() {
        let mut ex = Extractor::new().expect("build extractor");
        let (_, nodes, _) = extract_container("A.svelte", SVELTE, &mut ex);
        for bad in ["ghost", "str", "css"] {
            assert!(nodes.iter().all(|n| n.name != bad), "{bad} was indexed");
        }
        let (_, none, _) =
            extract_container("B.svelte", "<p>hi</p>\n<style>a{}</style>\n", &mut ex);
        assert!(none.is_empty());
    }

    #[test]
    fn test_svelte_5_module_attribute_script_is_indexed() {
        let src = "<script module>\nexport const f = () => 1;\n</script>\n";
        let mut ex = Extractor::new().expect("build extractor");
        let (_, nodes, _) = extract_container("M.svelte", src, &mut ex);
        assert!(nodes.iter().any(|n| n.name == "f" && n.span == "L2-L2"));
    }

    fn svelte_names(src: &str) -> Vec<(String, String)> {
        let mut ex = Extractor::new().expect("build extractor");
        names(&extract_container("A.svelte", src, &mut ex).1)
    }

    #[test]
    fn test_svelte_generics_with_angle_brackets_in_attribute() {
        let src = "<script lang=\"ts\" generics=\"T extends Array<string>\">\nfunction f<T>(x: T) {}\n</script>\n";
        assert_eq!(
            svelte_names(src),
            vec![("f".to_string(), "L2-L2".to_string())]
        );
    }

    #[test]
    fn test_svelte_arrow_in_attribute_does_not_end_the_tag() {
        let src = "<script lang=\"ts\" data-x='a => b'>\nfunction g() {}\n</script>\n";
        assert_eq!(
            svelte_names(src),
            vec![("g".to_string(), "L2-L2".to_string())]
        );
    }

    #[test]
    fn test_svelte_html_comment_with_script_and_brace_hides_nothing() {
        let src = "<!-- <script>function ghost() {}</script> { -->\n<script>\nfunction real() {}\n</script>\n";
        assert_eq!(
            svelte_names(src),
            vec![("real".to_string(), "L3-L3".to_string())]
        );
    }

    #[test]
    fn test_svelte_unclosed_brace_before_a_script_hides_nothing() {
        let src = "<p>{ oops</p>\n<script>\nfunction real() {}\n</script>\n";
        assert_eq!(
            svelte_names(src),
            vec![("real".to_string(), "L3-L3".to_string())]
        );
    }

    /// Checks that a brace in a string hides no script. A later stray `}`
    /// would close a wrong count and swallow the script.
    fn assert_script_found_after(expr: &str) {
        let src = format!("<p>{expr}</p>\n<script>\nfunction real() {{}}\n</script>\n<p>}}</p>\n");
        assert_eq!(
            svelte_names(&src),
            vec![("real".to_string(), "L3-L3".to_string())]
        );
    }

    #[test]
    fn test_svelte_expr_brace_in_double_quoted_string() {
        assert_script_found_after("{\"text with { brace\"}");
    }

    #[test]
    fn test_svelte_expr_brace_in_single_quoted_string() {
        assert_script_found_after("{a ? '{' : b}");
    }

    #[test]
    fn test_svelte_expr_brace_in_template_string() {
        assert_script_found_after("{`a { ${b}`}");
    }

    #[test]
    fn test_svelte_expr_escaped_quote_in_string() {
        assert_script_found_after("{\"a \\\" { b\"}");
    }

    #[test]
    fn test_astro_apostrophe_in_markup_does_not_open_a_string() {
        let src =
            "{a(<b>don't</b>)}\n<script>\nfunction real() {}\n</script>\n<p>it's</p>\n<p>}</p>\n";
        let mut ex = Extractor::new().expect("build extractor");
        let got = names(&extract_container("P.astro", src, &mut ex).1);
        assert_eq!(got, vec![("real".to_string(), "L3-L3".to_string())]);
    }

    #[test]
    fn test_svelte_non_ascii_markup_does_not_panic() {
        let src = "<p>\u{2022} caf\u{e9}</p>\n<script>\nfunction real() {}\n</script>\n";
        assert_eq!(
            svelte_names(src),
            vec![("real".to_string(), "L3-L3".to_string())]
        );
    }

    #[test]
    fn test_svelte_crlf_line_ends_give_real_lines() {
        let src = "<p>x</p>\r\n<script>\r\nfunction real() {}\r\n</script>\r\n";
        assert_eq!(
            svelte_names(src),
            vec![("real".to_string(), "L3-L3".to_string())]
        );
    }

    #[test]
    fn test_svelte_script_src_with_empty_body_gives_no_block() {
        let src = "<script src=\"x.ts\"></script>\n<p>x</p>\n";
        assert!(scan_blocks_for("svelte", src).is_empty());
    }

    const ASTRO: &str = "---\nimport x from './x';\nexport function title(): string {\n  return 'a';\n}\n---\n<h1>{title()}</h1>\n<script>\n  function onClick(): void {}\n</script>\n";

    #[test]
    fn test_astro_frontmatter_and_script_give_nodes_on_real_lines() {
        let mut ex = Extractor::new().expect("build extractor");
        let (file, nodes, _) = extract_container("P.astro", ASTRO, &mut ex);
        let got = names(&nodes);
        assert!(got.contains(&("title".into(), "L3-L5".into())), "{got:?}");
        assert!(got.contains(&("onClick".into(), "L9-L9".into())), "{got:?}");
        assert_eq!(file.body_hash, hex_sha256(ASTRO.as_bytes()));
    }

    #[test]
    fn test_astro_markup_only_and_unclosed_fence_give_no_nodes() {
        let mut ex = Extractor::new().expect("build extractor");
        let (_, a, _) = extract_container("P.astro", "<h1>hi</h1>\n", &mut ex);
        let (_, b, _) = extract_container("Q.astro", "---\nfunction f() {}\n", &mut ex);
        assert!(a.is_empty() && b.is_empty());
    }

    #[test]
    fn scan_blocks_finds_the_script_body_after_the_opening_tag() {
        let source = "<template></template>\n<script lang=\"ts\">\nconst x = 1;\n</script>\n";
        let blocks = scan_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "\nconst x = 1;\n");
        assert_eq!(blocks[0].start_row, 1);
    }

    #[test]
    fn scan_blocks_does_not_match_a_script_editor_tag() {
        // A Vue grammar only ever names a `script_element` node; a
        // `<script-editor>` node has a different type and is never one.
        let source = "<script-editor></script-editor>\n<script>\nconst x = 1;\n</script>\n";
        let blocks = scan_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "\nconst x = 1;\n");
    }

    #[test]
    fn scan_blocks_ignores_a_literal_script_tag_inside_a_template() {
        // A Vue grammar nests this text under `template_element`, so it
        // is never a `script_element` at the document root, no matter what
        // text it contains.
        let source = "<template>const s = '<script>oops</script>';</template>\n<script>\nconst x = 1;\n</script>\n";
        let blocks = scan_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "\nconst x = 1;\n");
    }

    #[test]
    fn scan_blocks_skips_an_empty_script() {
        let source = "<script></script>\n<script>\nconst x = 1;\n</script>\n";
        let blocks = scan_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "\nconst x = 1;\n");
    }

    #[test]
    fn shift_span_adds_lines_to_both_ends() {
        assert_eq!(shift_span("L1-L2", 5), "L6-L7");
        assert_eq!(shift_span("not-a-span", 5), "not-a-span");
    }

    #[test]
    fn extract_container_p2_05_09_10_shifts_spans_and_hashes_the_whole_file() {
        let source = "<template></template>\n\n<script lang=\"ts\">\nfunction run(): number {\n  return 1;\n}\n</script>\n";
        let mut extractor = Extractor::new().expect("build extractor");
        let (file_node, symbols, _edges) = extract_container("a.vue", source, &mut extractor);
        assert_eq!(file_node.id, "a.vue");
        assert_eq!(file_node.origin, Origin::Ast);
        assert_eq!(file_node.chars, Some(source.encode_utf16().count() as u64));
        assert_eq!(file_node.body_hash, hex_sha256(source.as_bytes()));
        let run = symbols
            .iter()
            .find(|n| n.name == "run")
            .expect("run symbol");
        // The script body starts at row 2 (0-indexed), so the function's
        // line 2 inside the script lands on line 4 of the .vue file.
        assert_eq!(run.span, "L4-L6");
    }
}
