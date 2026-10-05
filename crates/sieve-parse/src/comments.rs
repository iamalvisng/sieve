//! Comment blocks from the syntax tree. `sieve why` uses them for its
//! reason-comment rows. A block comes only from a comment node of the
//! grammar that the extractor uses for the file. Text in a string literal
//! is never a comment node.

use std::path::Path;

use tree_sitter::{Language, Node as TsNode, Parser};

/// The tree-sitter language of a depth-tier grammar name.
fn depth_language(grammar: &str) -> Option<Language> {
    Some(match grammar {
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "r" => tree_sitter_r::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_sg::LANGUAGE.into(),
        _ => return None,
    })
}

/// The language of `path`: the depth tier first, then the breadth tier.
fn language_of(path: &str) -> Option<Language> {
    match sieve_core::lang::lang_for_path(Path::new(path)) {
        Some(lang) => depth_language(lang.grammar),
        None => {
            crate::generic::generic_lang_of(path).and_then(|l| crate::generic::language_for(l.name))
        }
    }
}

/// Removes the comment markers from one line of a comment node.
fn strip_markers(line: &str) -> &str {
    let mut t = line.trim();
    for marker in ["///", "//!", "//", "/**", "/*", "*/", "--", "*", "#"] {
        if let Some(rest) = t.strip_prefix(marker) {
            t = rest.trim_start();
            break;
        }
    }
    t.trim_end_matches("*/").trim()
}

/// Collects the outermost comment nodes under `node`, as
/// `(first row, last row, standalone, text)`. A node is a comment when its
/// kind name holds `comment`. A comment is standalone when only blanks
/// come before it on its first line.
fn collect(node: TsNode, source: &str, out: &mut Vec<(usize, usize, bool, String)>) {
    if node.kind().contains("comment") {
        let (a, b) = (node.start_position(), node.end_position());
        let line_start = source[..node.start_byte()].rfind('\n').map_or(0, |p| p + 1);
        let standalone = source[line_start..node.start_byte()].trim().is_empty();
        let text = source
            .get(node.start_byte()..node.end_byte())
            .unwrap_or("")
            .lines()
            .map(strip_markers)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        out.push((a.row + 1, b.row + 1, standalone, text));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, out);
    }
}

/// The comment blocks that lie inside lines `first` to `last` (1-based) of
/// `source`, as `(first line, text)`. Adjacent standalone comments join into
/// one block. Gives `None` when `path` has no parser.
pub fn comment_blocks(
    path: &str,
    source: &str,
    first: usize,
    last: usize,
) -> Option<Vec<(usize, String)>> {
    let mut parser = Parser::new();
    let mut found = Vec::new();
    if let Some(lang) = crate::container::container_lang_of(path) {
        // A container file: read the comments of each script block, and
        // shift their rows to the lines of the whole file.
        parser.set_language(&depth_language("typescript")?).ok()?;
        for block in crate::container::scan_blocks_for(lang.name, source) {
            let Some(tree) = parser.parse(&block.text, None) else {
                continue; // one block that fails to parse hides no other block
            };
            let mut inner = Vec::new();
            collect(tree.root_node(), &block.text, &mut inner);
            let shift = block.start_row as usize;
            found.extend(
                inner
                    .into_iter()
                    .map(|(a, b, s, t)| (a + shift, b + shift, s, t)),
            );
        }
    } else {
        parser.set_language(&language_of(path)?).ok()?;
        let tree = parser.parse(source, None)?;
        collect(tree.root_node(), source, &mut found);
    }
    let mut blocks: Vec<(usize, usize, bool, String)> = Vec::new();
    for c in found.into_iter().filter(|c| first <= c.0 && c.1 <= last) {
        match blocks.last_mut() {
            Some(prev) if prev.2 && c.2 && c.0 == prev.1 + 1 => {
                prev.1 = c.1;
                prev.3.push(' ');
                prev.3.push_str(&c.3);
            }
            _ => blocks.push(c),
        }
    }
    Some(blocks.into_iter().map(|b| (b.0, b.3)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f6_comment_blocks_join_adjacent_lines_and_skip_strings() {
        let src = "fn a() {\n    // SAFETY: the lock\n    // is held.\n    let s = \"// not a comment\";\n    let t = 1; // because x\n    /* never\n       twice */\n}\n";
        let blocks = comment_blocks("a.rs", src, 1, 8).expect("rust parser");
        assert_eq!(
            blocks,
            vec![
                (2, "SAFETY: the lock is held.".to_string()),
                (5, "because x".to_string()),
                (6, "never twice".to_string()),
            ]
        );
        // A comment outside the range is not returned.
        assert_eq!(comment_blocks("a.rs", src, 5, 5).expect("rust").len(), 1);
    }

    #[test]
    fn test_f6_comment_blocks_read_python_and_skip_a_language_with_no_parser() {
        let src = "def f():\n    # because y\n    x = '# no'\n";
        let blocks = comment_blocks("a.py", src, 1, 3).expect("python parser");
        assert_eq!(blocks, vec![(2, "because y".to_string())]);
        assert_eq!(comment_blocks("a.unknownext", src, 1, 3), None);
    }
}
