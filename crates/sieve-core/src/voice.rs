//! The row grammar every query command prints: `name  kind  path:start-end`.

/// Shortens a kind word for a row: `function` is `fn`, `interface` is
/// `iface`, and so on. A word with no short form stays as it is.
pub fn short_kind(word: &str) -> &str {
    match word {
        "function" => "fn",
        "interface" => "iface",
        "constant" => "const",
        "variable" => "var",
        other => other,
    }
}

/// Turns a span like `L411-L679` into `411-679`. Text that is not a span
/// stays as it is.
pub fn range(span: &str) -> String {
    let Some((start, end)) = span.split_once('-') else {
        return span.strip_prefix('L').unwrap_or(span).to_string();
    };
    let start = start.strip_prefix('L').unwrap_or(start);
    let end = end.strip_prefix('L').unwrap_or(end);
    format!("{start}-{end}")
}

/// Turns a pointer like `path:L411-L679` into `path:411-679`.
pub fn pointer(pointer: &str) -> String {
    match pointer.rsplit_once(":L") {
        Some((path, span)) if span.starts_with(|c: char| c.is_ascii_digit()) => {
            format!("{path}:{}", range(&format!("L{span}")))
        }
        _ => pointer.to_string(),
    }
}

/// Builds one row: `name  kind  path:start-end`. A row with no span ends
/// with the path.
pub fn row(name: &str, kind_word: &str, path: &str, span: Option<&str>) -> String {
    let kind = short_kind(kind_word);
    match span {
        Some(span) => format!("{name}  {kind}  {path}:{}", range(span)),
        None => format!("{name}  {kind}  {path}"),
    }
}

/// Writes a count with its noun: `1 symbol`, `2 symbols`.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Splits a hit title `name · kind` into its two parts. A title with no
/// separator has no kind.
pub fn split_title(title: &str) -> (&str, Option<&str>) {
    match title.rsplit_once(" \u{b7} ") {
        Some((name, kind)) => (name, Some(kind)),
        None => (title, None),
    }
}

/// One line of a tree: the row text, the id of the row above it in the tree,
/// and an optional extra line that goes under the row.
#[derive(Debug, Clone)]
pub struct TreeItem {
    /// The id other items name as their parent.
    pub id: String,
    /// The id of the parent item. An item whose parent is not in the list is
    /// a root.
    pub parent: Option<String>,
    /// The row text.
    pub text: String,
    /// A line under the row, such as a call line.
    pub note: Option<String>,
}

/// Draws `items` as a tree with `\u{251c}\u{2500}`, `\u{2514}\u{2500}` and `\u{2502}`. Roots
/// and children keep the order of `items`.
pub fn tree_lines(items: &[TreeItem]) -> Vec<String> {
    use std::collections::{HashMap, HashSet};
    let ids: HashSet<&str> = items.iter().map(|i| i.id.as_str()).collect();
    let mut kids: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (n, item) in items.iter().enumerate() {
        match item.parent.as_deref() {
            Some(p) if p != item.id && ids.contains(p) => kids.entry(p).or_default().push(n),
            _ => roots.push(n),
        }
    }
    let mut lines = Vec::new();
    // Each frame: the item index, the prefix of its line, and whether it is
    // the last child of its parent.
    let mut stack: Vec<(usize, String, bool)> = Vec::new();
    for (k, &n) in roots.iter().enumerate().rev() {
        stack.push((n, String::new(), k + 1 == roots.len()));
    }
    while let Some((n, prefix, last)) = stack.pop() {
        let item = &items[n];
        let (branch, pad) = if last {
            ("\u{2514}\u{2500} ", "   ")
        } else {
            ("\u{251c}\u{2500} ", "\u{2502}  ")
        };
        lines.push(format!("{prefix}{branch}{}", item.text));
        let child_prefix = format!("{prefix}{pad}");
        let children = kids.get(item.id.as_str()).map_or(&[][..], Vec::as_slice);
        if let Some(note) = &item.note {
            let bar = if children.is_empty() {
                "   "
            } else {
                "\u{2502}  "
            };
            lines.push(format!("{child_prefix}{bar}{note}"));
        }
        for (k, &child) in children.iter().enumerate().rev() {
            stack.push((child, child_prefix.clone(), k + 1 == children.len()));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_uses_short_kinds_and_bare_ranges() {
        assert_eq!(
            row("handleHMRUpdate", "function", "a/hmr.ts", Some("L411-L679")),
            "handleHMRUpdate  fn  a/hmr.ts:411-679"
        );
        // A file keeps its range. The end line is the last line of the span.
        assert_eq!(
            row("m.mjs", "file", "p/m.mjs", Some("L1-L9")),
            "m.mjs  file  p/m.mjs:1-9"
        );
        assert_eq!(range("L411-L679"), "411-679");
        assert_eq!(count(1, "symbol"), "1 symbol");
        assert_eq!(count(0, "symbol"), "0 symbols");
        assert_eq!(row("T", "interface", "t.ts", None), "T  iface  t.ts");
    }

    #[test]
    fn pointer_drops_the_l_prefix_only_from_a_span() {
        assert_eq!(pointer("src/a.ts:L1-L5"), "src/a.ts:1-5");
        assert_eq!(pointer("src/Lib.ts"), "src/Lib.ts");
        assert_eq!(pointer("a.ts, b.ts"), "a.ts, b.ts");
    }

    #[test]
    fn split_title_reads_the_last_separator() {
        assert_eq!(
            split_title("a \u{b7} b \u{b7} function"),
            ("a \u{b7} b", Some("function"))
        );
        assert_eq!(split_title("concept"), ("concept", None));
    }

    #[test]
    fn tree_lines_nest_a_child_and_put_the_note_under_its_row() {
        let item = |id: &str, parent: Option<&str>, note: Option<&str>| TreeItem {
            id: id.to_string(),
            parent: parent.map(str::to_string),
            text: id.to_string(),
            note: note.map(str::to_string),
        };
        let lines = tree_lines(&[
            item("a", None, Some("9: x()")),
            item("b", Some("a"), None),
            item("c", None, Some("4: y()")),
        ]);
        assert_eq!(
            lines,
            [
                "\u{251c}\u{2500} a",
                "\u{2502}  \u{2502}  9: x()",
                "\u{2502}  \u{2514}\u{2500} b",
                "\u{2514}\u{2500} c",
                "      4: y()",
            ]
        );
    }
}
