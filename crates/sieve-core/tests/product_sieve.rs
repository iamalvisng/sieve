//! Pins that a `sieve/` context dir builds runtime strings that carry
//! `sieve`, and never the old name.
//!
//! Each expected literal below is the matching golden or unit-test
//! constant in `ignore.rs` or `cards.rs`.

use std::fs;
use std::path::Path;

/// A temp dir unique per test, removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("sieve-core-product-{label}-{pid}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `ignore.rs`'s `GITIGNORE_BLOCK` test constant.
const GITIGNORE_BLOCK_SIEVE: &str =
    "# sieve's local graph cache — regenerable, not committed (run `sieve build`).\n/sieve/\n";

/// `ignore.rs`'s `IGNORE_BLOCK` test constant.
const IGNORE_BLOCK_SIEVE: &str = "# sieve's cards are gitignored but should stay greppable: ripgrep reads\n# .ignore before .gitignore, so this re-admits the tree to search only.\n!sieve/\nsieve/.cache/\nsieve/.graph/\n";

/// `tests/fixtures/basic.expected/sieve/INDEX.md` with
/// the file/symbol counts set to 1, matching a `CardStats` of
/// `{ written: 1, with_symbols: 1 }` on an empty context dir (no concepts).
const INDEX_MD_SIEVE: &str = "# sieve — repo map\n\
\n\
Small markdown nodes summarising this repo. `grep` any term, symbol, or\n\
filename here, or run `sieve ask \"<task>\"`. Each node carries prose plus exact\n\
`file:line`; open a source file only to edit the named span.\n\
\n\
The same graph is queryable as MCP tools (`sieve_find_code`, `sieve_find_all`,\n\
`sieve_trace_calls`, `sieve_file_api`, `sieve_repo_map`) where a host exposes them, and\n\
as the `sieve` CLI everywhere else. Edges — who calls what — live only in the\n\
graph, not in these files: `sieve callers <symbol>` is the only way to read them.\n\
\n\
## Files\n\
\n\
1 per-file wiring cards mirror the source tree under `sieve/` (1 carry extracted symbols). They are deliberately not enumerated here —\n\
`grep` a symbol or `find`/`ls` a filename under `sieve/` to land on the card for that file.\n";

#[test]
fn rename_table_core_ignore_block_holds_sieve_not_old_name() {
    let root = TempDir::new("ignore");
    let context_dir = root.join("sieve");

    sieve_core::ignore::ensure_gitignored(&root, &context_dir, false).expect("gitignore succeeds");
    let gitignore = fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
    assert_eq!(gitignore, GITIGNORE_BLOCK_SIEVE);

    sieve_core::ignore::ensure_searchable(&root, &context_dir, false).expect("ignore succeeds");
    let ignore = fs::read_to_string(root.join(".ignore")).expect("read ignore");
    assert_eq!(ignore, IGNORE_BLOCK_SIEVE);
}

#[test]
fn rename_table_core_card_index_holds_sieve_not_old_name() {
    let root = TempDir::new("index");
    let context_dir = root.join("sieve");
    fs::create_dir_all(&context_dir).expect("create context dir");

    let stats = sieve_core::cards::CardStats {
        written: 1,
        files: 1,
        with_symbols: 1,
    };
    sieve_core::cards::write_index(&context_dir, &stats).expect("write_index succeeds");
    let text = fs::read_to_string(context_dir.join("INDEX.md")).expect("read INDEX.md");
    assert_eq!(text, INDEX_MD_SIEVE);
}
