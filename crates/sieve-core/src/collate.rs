//! ICU root collation, to match Node's `String.prototype.localeCompare`
//! with no arguments (section 0 of the build-cards-freshness note).

use std::cmp::Ordering;
use std::sync::OnceLock;

use icu_collator::{options::CollatorOptions, Collator, CollatorBorrowed, CollatorPreferences};

static COLLATOR: OnceLock<Option<CollatorBorrowed<'static>>> = OnceLock::new();

/// Builds the shared root-locale collator once, on first use.
///
/// The root locale, default strength and default case handling here match
/// Node's `localeCompare()` with no arguments. The exact match is verified
/// against Node for the pairs in `collate_matches_node_locale_compare`.
fn collator() -> Option<&'static CollatorBorrowed<'static>> {
    COLLATOR
        .get_or_init(|| {
            Collator::try_new(CollatorPreferences::default(), CollatorOptions::default()).ok()
        })
        .as_ref()
}

/// Compares `a` and `b` with ICU root collation, matching Node's
/// `a.localeCompare(b)` with no arguments.
///
/// Falls back to byte order when the collator data fails to load, which
/// does not happen with the compiled-data feature this crate uses.
pub fn collate(a: &str, b: &str) -> Ordering {
    match collator() {
        Some(collator) => collator.compare(a, b),
        None => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collate_matches_node_locale_compare() {
        let mut items = vec![
            "Public",
            "_Private",
            "_helper",
            "a.b",
            "a#b",
            "a~2",
            "a",
            "A",
            "b",
            "B",
            "src/app.ts#App.run",
            "src/app.ts#App",
            "src/app.ts",
        ];
        items.sort_by(|a, b| collate(a, b));
        assert_eq!(
            items,
            vec![
                "_helper",
                "_Private",
                "a",
                "A",
                "a.b",
                "a#b",
                "a~2",
                "b",
                "B",
                "Public",
                "src/app.ts",
                "src/app.ts#App",
                "src/app.ts#App.run",
            ]
        );
    }

    /// P2-06 (DV5): scope prefixes of equal UTF-16 length sort by `collate`, as
    /// JS `a.prefix.localeCompare(b.prefix)` does. The expected order is Node
    /// 24 `localeCompare` output. A run over 2744 three-unit strings (3,763,396
    /// pairs) found 0 mismatches.
    #[test]
    fn test_p2_06_dv5_equal_length_prefixes_sort_like_locale_compare() {
        let node_order = [
            "_ab", "-ab", ".ab", "1ab", "a_b", "a_B", "ä_b", "a-b", "a-B", "a.b", "a/b", "a1b",
            "a9b", "aab", "aAb", "Aab", "aäb", "äab", "ab1", "eab", "Eab", "éab", "zab", "Zab",
        ];
        let mut items = node_order.to_vec();
        items.reverse();
        items.sort_by(|a, b| collate(a, b));
        assert_eq!(items, node_order);
    }

    #[test]
    fn collate_is_consistent_with_equal_strings() {
        assert_eq!(collate("a", "a"), Ordering::Equal);
    }
}
