//! The native-tier language table. Sieve picks a grammar and a display
//! label for a source file by its extension, longest suffix first.
//!
//! This table covers only the native (depth) tier. The container tier
//! (`.vue`) and the breadth tier come later.

use std::path::Path;

/// One language entry: the grammar Sieve parses with, and the label it
/// reports to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lang {
    pub grammar: &'static str,
    pub label: &'static str,
}

/// The extension table, longest suffix first, most-specific match wins.
const TABLE: &[(&str, Lang)] = &[
    (
        ".d.ts",
        Lang {
            grammar: "typescript",
            label: "typescript",
        },
    ),
    (
        ".tsx",
        Lang {
            grammar: "tsx",
            label: "tsx",
        },
    ),
    (
        ".jsx",
        Lang {
            grammar: "tsx",
            label: "jsx",
        },
    ),
    (
        ".mts",
        Lang {
            grammar: "typescript",
            label: "typescript",
        },
    ),
    (
        ".cts",
        Lang {
            grammar: "typescript",
            label: "typescript",
        },
    ),
    (
        ".ts",
        Lang {
            grammar: "typescript",
            label: "typescript",
        },
    ),
    (
        ".mjs",
        Lang {
            grammar: "typescript",
            label: "javascript",
        },
    ),
    (
        ".cjs",
        Lang {
            grammar: "typescript",
            label: "javascript",
        },
    ),
    (
        ".js",
        Lang {
            grammar: "typescript",
            label: "javascript",
        },
    ),
    (
        ".pyi",
        Lang {
            grammar: "python",
            label: "python",
        },
    ),
    (
        ".py",
        Lang {
            grammar: "python",
            label: "python",
        },
    ),
    (
        ".go",
        Lang {
            grammar: "go",
            label: "go",
        },
    ),
    (
        ".java",
        Lang {
            grammar: "java",
            label: "java",
        },
    ),
    (
        ".kt",
        Lang {
            grammar: "kotlin",
            label: "kotlin",
        },
    ),
    (
        ".kts",
        Lang {
            grammar: "kotlin",
            label: "kotlin",
        },
    ),
    (
        ".swift",
        Lang {
            grammar: "swift",
            label: "swift",
        },
    ),
    (
        ".php",
        Lang {
            grammar: "php",
            label: "php",
        },
    ),
];

/// Finds the language for one file path, by its longest matching suffix.
///
/// Sieve lower-cases the file name before matching, so every suffix
/// matches case-insensitively. Returns `None` when no suffix matches.
pub fn lang_for_path(path: &Path) -> Option<Lang> {
    let name = path.file_name()?.to_str()?.to_lowercase();
    let name = name.as_str();

    if name.len() >= 2 {
        let (stem, ext) = name.split_at(name.len() - 2);
        if ext.eq_ignore_ascii_case(".r") && !stem.is_empty() {
            return Some(Lang {
                grammar: "r",
                label: "r",
            });
        }
    }

    TABLE
        .iter()
        .filter(|(suffix, _)| name.len() > suffix.len() && name.ends_with(suffix))
        .max_by_key(|(suffix, _)| suffix.len())
        .map(|(_, lang)| *lang)
}

/// Every extension the native tier claims, in table order, for the `-e`
/// warning's `supported:` list. A row that only refines a shorter row (`.d.ts`
/// under `.ts`) is left out: Sieve has no such row, and the shorter suffix
/// already claims the file. `.r` is listed too; `lang_for_path` matches it
/// outside the table.
pub fn native_extensions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = TABLE
        .iter()
        .map(|(suffix, _)| *suffix)
        .filter(|suffix| {
            !TABLE
                .iter()
                .any(|(other, _)| other.len() < suffix.len() && suffix.ends_with(other))
        })
        .collect();
    if !out.contains(&".r") {
        out.push(".r");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_extensions_skip_the_dts_refinement_and_hold_r() {
        let exts = native_extensions();
        assert!(!exts.contains(&".d.ts"), "{exts:?}");
        assert!(exts.contains(&".ts"));
        assert_eq!(exts.iter().filter(|e| **e == ".r").count(), 1);
    }

    #[test]
    fn dts_gives_typescript_by_longest_suffix() {
        let lang = lang_for_path(Path::new("a.d.ts")).expect("a.d.ts should match");
        assert_eq!(lang.grammar, "typescript");
        assert_eq!(lang.label, "typescript");
    }

    #[test]
    fn mjs_gives_typescript_grammar_and_javascript_label() {
        let lang = lang_for_path(Path::new("a.mjs")).expect("a.mjs should match");
        assert_eq!(lang.grammar, "typescript");
        assert_eq!(lang.label, "javascript");
    }

    #[test]
    fn r_matches_case_insensitively() {
        let upper = lang_for_path(Path::new("a.R")).expect("a.R should match");
        let lower = lang_for_path(Path::new("a.r")).expect("a.r should match");
        assert_eq!(upper.grammar, "r");
        assert_eq!(lower.grammar, "r");
    }

    #[test]
    fn uppercase_ts_matches_typescript() {
        let lang = lang_for_path(Path::new("a.TS")).expect("a.TS should match");
        assert_eq!(lang.grammar, "typescript");
        assert_eq!(lang.label, "typescript");
    }

    #[test]
    fn unknown_extension_gives_none() {
        assert_eq!(lang_for_path(Path::new("a.md")), None);
    }
}
