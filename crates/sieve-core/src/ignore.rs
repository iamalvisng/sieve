//! The `.gitignore` and `.ignore` footer edits Sieve makes next to a
//! context dir it did not put there (section 6 of the build-cards-freshness
//! note).

use std::fs;
use std::io;
use std::path::Path;

/// Returns the repo-relative, forward-slash path from `root` to
/// `context_dir`, or `None` when `context_dir` is outside `root`.
fn relative_slash_path(root: &Path, context_dir: &Path) -> Option<String> {
    let rel = context_dir.strip_prefix(root).ok()?;
    let rel = rel.to_str()?.replace('\\', "/");
    if rel.is_empty() || rel.starts_with("..") {
        return None;
    }
    Some(rel)
}

/// The `.gitignore` block `ensure_gitignored` writes for the context dir
/// `bare`, branded for the active product.
pub fn gitignore_block(bare: &str) -> String {
    let name = crate::product().name;
    format!(
        "# {name}'s local graph cache — regenerable, not committed (run `{name} build`).\n/{bare}/\n"
    )
}

/// The `.ignore` block `ensure_searchable` writes for the context dir
/// `bare`, branded for the active product.
pub fn ignore_block(bare: &str) -> String {
    let name = crate::product().name;
    format!(
        "# {name}'s cards are gitignored but should stay greppable: ripgrep reads\n# .ignore before .gitignore, so this re-admits the tree to search only.\n!{bare}/\n{bare}/.cache/\n{bare}/.graph/\n"
    )
}

/// Ensures `<root>/.gitignore` carries an entry for the context dir.
///
/// Does nothing when `skip` is `true`, when `context_dir` is not under
/// `root`, or when an accepted entry (`/<bare>/`, `<bare>/`, or `<bare>`) is
/// already present. Write failures are swallowed; only a
/// failure to create the parent directory returns an error.
pub fn ensure_gitignored(root: &Path, context_dir: &Path, skip: bool) -> io::Result<()> {
    ensure_entry(root, context_dir, skip, ".gitignore", |bare| {
        let accepted = vec![format!("/{bare}/"), format!("{bare}/"), bare.to_string()];
        (accepted, gitignore_block(bare))
    })
}

/// Ensures `<root>/.ignore` re-admits the context dir to search tools.
///
/// Does nothing when `skip` is `true`, when `context_dir` is not under
/// `root`, or when the accepted entry (`!<bare>/`) is already present.
pub fn ensure_searchable(root: &Path, context_dir: &Path, skip: bool) -> io::Result<()> {
    ensure_entry(root, context_dir, skip, ".ignore", |bare| {
        let accepted = vec![format!("!{bare}/")];
        (accepted, ignore_block(bare))
    })
}

/// Shared logic for `ensure_gitignored` and `ensure_searchable`: read, check
/// for an accepted entry, then append the block with the right gap.
///
/// `entry_for` derives the accepted forms and the block from `bare`, the
/// context dir's repo-relative path.
fn ensure_entry(
    root: &Path,
    context_dir: &Path,
    skip: bool,
    file_name: &str,
    entry_for: impl Fn(&str) -> (Vec<String>, String),
) -> io::Result<()> {
    if skip {
        return Ok(());
    }
    let Some(rel) = relative_slash_path(root, context_dir) else {
        return Ok(());
    };
    // The whole repo-relative path, without trailing slashes:
    // `tools/ctx` stays `tools/ctx`.
    let bare = rel.trim_end_matches('/');
    let (accepted, block) = entry_for(bare);

    let path = root.join(file_name);
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let has_existing_file = path.exists();

    if accepted
        .iter()
        .any(|form| existing.lines().any(|line| line.trim() == form.as_str()))
    {
        return Ok(());
    }

    let gap = if existing.is_empty() {
        ""
    } else if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let new_text = format!("{existing}{gap}{block}");

    if !has_existing_file {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
    }
    let _ = fs::write(&path, new_text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default `sieve` context dir's gitignore block, byte-identical to
    /// the block the P1 build parity test pins.
    const GITIGNORE_BLOCK: &str =
        "# sieve's local graph cache — regenerable, not committed (run `sieve build`).\n/sieve/\n";

    /// The default `sieve` context dir's ignore block, byte-identical to
    /// the block the P1 build parity test pins.
    const IGNORE_BLOCK: &str = "# sieve's cards are gitignored but should stay greppable: ripgrep reads\n# .ignore before .gitignore, so this re-admits the tree to search only.\n!sieve/\nsieve/.cache/\nsieve/.graph/\n";

    /// A temp dir unique per call, that removes itself on drop, even if
    /// the test panics before it reaches its own cleanup line.
    fn temp_dir(tag: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(tag)
    }

    #[test]
    fn ensure_gitignored_creates_the_file_with_the_block_alone() {
        let root = temp_dir("create");
        let context_dir = root.join("sieve");

        ensure_gitignored(&root, &context_dir, false).expect("ensure succeeds");
        let text = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
        assert_eq!(text, GITIGNORE_BLOCK);
    }

    #[test]
    fn ensure_gitignored_appends_one_blank_line_when_the_file_ends_with_a_newline() {
        let root = temp_dir("trailing-newline");
        fs::write(root.join(".gitignore"), "node_modules/\n").expect("write existing");
        let context_dir = root.join("sieve");

        ensure_gitignored(&root, &context_dir, false).expect("ensure succeeds");
        let text = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
        assert_eq!(text, format!("node_modules/\n\n{GITIGNORE_BLOCK}"));
    }

    #[test]
    fn ensure_gitignored_appends_two_newlines_when_the_file_lacks_a_trailing_newline() {
        let root = temp_dir("no-trailing-newline");
        fs::write(root.join(".gitignore"), "node_modules/").expect("write existing");
        let context_dir = root.join("sieve");

        ensure_gitignored(&root, &context_dir, false).expect("ensure succeeds");
        let text = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
        assert_eq!(text, format!("node_modules/\n\n{GITIGNORE_BLOCK}"));
    }

    #[test]
    fn ensure_gitignored_treats_each_accepted_form_as_a_no_op() {
        for form in ["/sieve/", "sieve/", "sieve"] {
            let root = temp_dir(&format!("accepted-{}", form.replace('/', "-")));
            fs::write(root.join(".gitignore"), format!("{form}\n")).expect("write existing");
            let context_dir = root.join("sieve");

            ensure_gitignored(&root, &context_dir, false).expect("ensure succeeds");
            let text = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
            assert_eq!(text, format!("{form}\n"));
        }
    }

    #[test]
    fn ensure_gitignored_second_call_is_a_no_op() {
        let root = temp_dir("second-call");
        let context_dir = root.join("sieve");

        ensure_gitignored(&root, &context_dir, false).expect("first call succeeds");
        let first = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
        ensure_gitignored(&root, &context_dir, false).expect("second call succeeds");
        let second = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
        assert_eq!(first, second);
    }

    #[test]
    fn ensure_searchable_creates_the_file_with_the_block_alone() {
        let root = temp_dir("searchable-create");
        let context_dir = root.join("sieve");

        ensure_searchable(&root, &context_dir, false).expect("ensure succeeds");
        let text = fs::read_to_string(root.join(".ignore")).expect("read ignore");
        assert_eq!(text, IGNORE_BLOCK);
    }

    /// A `--dir tools/ctx` context dir gives the whole sub-path in both
    /// files.
    #[test]
    fn test_p2_32_dv11_a_sub_path_context_dir_keeps_the_whole_path() {
        let root = temp_dir("dv11");
        let context_dir = root.join("tools/ctx");

        ensure_gitignored(&root, &context_dir, false).expect("gitignore succeeds");
        let gitignore = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
        assert_eq!(
            gitignore,
            "# sieve's local graph cache — regenerable, not committed (run `sieve build`).\n/tools/ctx/\n"
        );

        ensure_searchable(&root, &context_dir, false).expect("ignore succeeds");
        let ignore = fs::read_to_string(root.join(".ignore")).expect("read ignore");
        assert_eq!(
            ignore,
            "# sieve's cards are gitignored but should stay greppable: ripgrep reads\n# .ignore before .gitignore, so this re-admits the tree to search only.\n!tools/ctx/\ntools/ctx/.cache/\ntools/ctx/.graph/\n"
        );

        // Each accepted form for the whole path is a no-op; the last
        // segment alone is not.
        for form in ["/tools/ctx/", "tools/ctx/", "tools/ctx"] {
            let again = temp_dir("dv11-accepted");
            fs::write(again.join(".gitignore"), format!("{form}\n")).expect("write existing");
            ensure_gitignored(&again, &again.join("tools/ctx"), false).expect("ensure succeeds");
            let text = fs::read_to_string(again.join(".gitignore")).expect("read gitignore");
            assert_eq!(text, format!("{form}\n"));
        }
        let last = temp_dir("dv11-last");
        fs::write(last.join(".gitignore"), "ctx/\n").expect("write existing");
        ensure_gitignored(&last, &last.join("tools/ctx"), false).expect("ensure succeeds");
        let text = fs::read_to_string(last.join(".gitignore")).expect("read gitignore");
        assert!(text.ends_with("/tools/ctx/\n"));
    }

    #[test]
    fn ensure_entry_derives_the_bare_name_from_a_non_default_context_dir() {
        let root = temp_dir("bare-sieve");
        let context_dir = root.join("sieve");

        ensure_gitignored(&root, &context_dir, false).expect("gitignore succeeds");
        let gitignore = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
        assert_eq!(
            gitignore,
            "# sieve's local graph cache — regenerable, not committed (run `sieve build`).\n/sieve/\n"
        );

        ensure_searchable(&root, &context_dir, false).expect("ignore succeeds");
        let ignore = fs::read_to_string(root.join(".ignore")).expect("read ignore");
        assert_eq!(
            ignore,
            "# sieve's cards are gitignored but should stay greppable: ripgrep reads\n# .ignore before .gitignore, so this re-admits the tree to search only.\n!sieve/\nsieve/.cache/\nsieve/.graph/\n"
        );
    }
}
