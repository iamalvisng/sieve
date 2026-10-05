//! Integration test for `sieve build` (P2-28): the golden wiring.json
//! write, byte for byte, and the missing-dir error.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use support::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let file_type = entry.file_type().expect("read file type");
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

#[test]
fn test_p2_28_build_writes_the_golden_wiring_json_byte_for_byte() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let expected = support::manifest_dir().join("../../tests/fixtures/basic.expected/wiring.json");
    let temp = TempDir::new("build-golden");
    copy_dir(&fixture, &temp.path);

    let output = support::sieve_command()
        .args(["build", temp.path.to_str().expect("utf-8 path")])
        .output()
        .expect("run sieve build");

    assert_eq!(output.status.code(), Some(0));
    // `build` now prints the Tier-1 report on stdout; the exact text is
    // pinned by `test_p2_26_28_29_30_31_32_build_tree_matches_golden`.
    assert!(!output.stdout.is_empty());

    let wiring_path = temp.path.join("sieve").join(".graph").join("wiring.json");
    let first_write = fs::read(&wiring_path).expect("read wiring.json");
    let expected_bytes = fs::read(&expected).expect("read expected wiring.json");
    assert_eq!(first_write, expected_bytes);

    // Run again: the write must stay byte-identical.
    let output = support::sieve_command()
        .args(["build", temp.path.to_str().expect("utf-8 path")])
        .output()
        .expect("run sieve build again");
    assert_eq!(output.status.code(), Some(0));
    let second_write = fs::read(&wiring_path).expect("read wiring.json again");
    assert_eq!(first_write, second_write);
}

#[test]
fn test_p3_02_build_writes_the_ask_sidecar() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let expected = support::manifest_dir().join("../../tests/fixtures/basic.expected/wiring.json");
    let temp = TempDir::new("build-ask-sidecar");
    copy_dir(&fixture, &temp.path);

    let output = support::sieve_command()
        .args(["build", temp.path.to_str().expect("utf-8 path")])
        .output()
        .expect("run sieve build");
    assert_eq!(output.status.code(), Some(0));

    let sidecar_path = temp
        .path
        .join("sieve")
        .join(".cache")
        .join("ask-index.json");
    let text = fs::read_to_string(&sidecar_path).expect("read ask-index.json");
    let index: serde_json::Value = serde_json::from_str(&text).expect("parse ask-index.json");

    assert_eq!(index["version"], 1);
    assert_eq!(index["docCount"], 21);
    assert_eq!(index["docs"].as_array().expect("docs array").len(), 21);

    let expected_bytes = fs::read(&expected).expect("read expected wiring.json");
    let expected_wiring: serde_json::Value =
        serde_json::from_slice(&expected_bytes).expect("parse expected wiring.json");
    let first_node_id = expected_wiring["nodes"][0]["id"].clone();
    assert_eq!(index["docs"][0]["id"], first_node_id);
}

#[test]
fn test_p3_02_refresh_rewrites_the_ask_sidecar() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("build-ask-sidecar-refresh");
    copy_dir(&fixture, &temp.path);

    let output = support::sieve_command()
        .args(["build", temp.path.to_str().expect("utf-8 path")])
        .output()
        .expect("run sieve build");
    assert_eq!(output.status.code(), Some(0));

    let sidecar_path = temp
        .path
        .join("sieve")
        .join(".cache")
        .join("ask-index.json");
    fs::remove_file(&sidecar_path).expect("remove ask-index.json");

    let util_path = temp.path.join("src/util.ts");
    let mut src = fs::read_to_string(&util_path).expect("read util.ts");
    src.push_str("\n// add: a marker comment for the refresh test.\n");
    fs::write(&util_path, src).expect("write util.ts");

    let output = support::sieve_command()
        .args(["grep", "add", "--fixed"])
        .current_dir(&temp.path)
        .output()
        .expect("run sieve grep");
    assert_eq!(output.status.code(), Some(0));

    let text = fs::read_to_string(&sidecar_path).expect("read rewritten ask-index.json");
    let index: serde_json::Value = serde_json::from_str(&text).expect("parse ask-index.json");
    assert_eq!(index["docCount"], 21);
}

/// P2-37: an explicit `sieve build` backfills the `covers:` block into a
/// hand-made concept file. A later plain query never rewrites that block
/// (deep-tier.md section 3: `writeCovers` runs only on an explicit build).
#[test]
fn test_p2_37_build_writes_covers_and_a_query_never_rewrites_them() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("build-covers");
    copy_dir(&fixture, &temp.path);

    let concept_path = temp.path.join("sieve").join("widgets.md");
    fs::create_dir_all(concept_path.parent().expect("concept parent dir"))
        .expect("create sieve dir");
    fs::write(
        &concept_path,
        "---\nslug: widgets\nname: Widgets\ntype: concept\nsources:\n  - path: src/util.ts\n    hash: x\n---\nbody\n",
    )
    .expect("write concept file");

    let (code, _, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code, Some(0), "first build exit code");

    let text = fs::read_to_string(&concept_path).expect("read concept file after build");
    let expected_frontmatter_tail = "covers:\n  - symbol: double\n    kind: function\n    at: 'src/util.ts:L1-L3'\n  - symbol: quadruple\n    kind: function\n    at: 'src/util.ts:L5-L7'\n---\n";
    assert!(
        text.contains(expected_frontmatter_tail),
        "concept file did not end its frontmatter with the covers block: {text:?}"
    );

    // A plain query never rewrites the covers block.
    let output = support::sieve_command()
        .args(["grep", "double", "--fixed"])
        .current_dir(&temp.path)
        .output()
        .expect("run sieve grep");
    assert_eq!(output.status.code(), Some(0));

    let text_after_query =
        fs::read_to_string(&concept_path).expect("read concept file after query");
    assert_eq!(
        text_after_query, text,
        "a plain query rewrote the covers block"
    );
}

#[test]
fn build_exits_1_on_a_missing_dir() {
    let temp = TempDir::new("build-missing");
    let missing = temp.path.join("does-not-exist");

    let output = support::sieve_command()
        .args(["build", missing.to_str().expect("utf-8 path")])
        .output()
        .expect("run sieve build");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert_eq!(
        drop_update_nudge(&stderr),
        format!(
            "ENOENT: no such file or directory, scandir '{}'\n",
            missing.display()
        )
    );
}

/// Prints `name` and a unified-looking line diff of `actual` against
/// `expected`, so a mismatch reports something readable.
fn print_diff(name: &str, actual: &str, expected: &str) {
    eprintln!("--- mismatch: {name} ---");
    let actual_lines: Vec<&str> = actual.lines().collect();
    let expected_lines: Vec<&str> = expected.lines().collect();
    let max = actual_lines.len().max(expected_lines.len());
    for i in 0..max {
        let a = actual_lines.get(i).copied().unwrap_or("<missing>");
        let e = expected_lines.get(i).copied().unwrap_or("<missing>");
        if a != e {
            eprintln!("- expected[{i}]: {e:?}");
            eprintln!("+ actual[{i}]:   {a:?}");
        }
    }
}

/// Replaces every run of `"../"` (zero or more), an optional leading `/`,
/// then the literal `body`, with `<TMP>`. It is a plain `sed`-style mask,
/// mask, without a regex dependency.
fn mask_tmp(text: &str, body: &str) -> String {
    let bytes = text.as_bytes();
    let body_bytes = body.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(body_bytes) {
            // Walk the already-written tail of `out` backward over any
            // "../" runs, then one optional leading "/".
            let mut cut = out.len();
            while out[..cut].ends_with("../") {
                cut -= 3;
            }
            if out[..cut].ends_with('/') {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str("<TMP>");
            i += body_bytes.len();
        } else {
            let ch = text[i..].chars().next().expect("valid utf-8 boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Runs `sieve build` against `dir` with `extra_args`, and returns
/// `(status_code, stdout, stderr)`.
fn run_build(
    dir: &Path,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> (Option<i32>, String, String) {
    let mut cmd = support::sieve_command();
    cmd.arg("build").arg(dir);
    cmd.args(extra_args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("run sieve build");
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
    )
}

/// Drops every line in `text` that starts with `⬆`, matching the update
/// nudge the test harness cannot silence.
fn drop_update_nudge(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with('⬆'))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Recursively collects every `.md` path, relative to `dir`, under `dir`.
///
/// A failed `read_dir` panics with the path instead of returning an empty
/// list, so a missing `sieve/` tree fails loudly instead of making the
/// card comparison below pass on two empty lists.
fn collect_md(dir: &Path, prefix: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("read dir {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        let rel = prefix.join(entry.file_name());
        if path.is_dir() {
            collect_md(&path, &rel, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(rel);
        }
    }
}

/// With `SIEVE_BLESS=1`, writes every golden file of one fixture from the
/// run in `dir`: the wiring graph, the ask sidecar, the cards, the ignore
/// files and the masked build output.
fn bless_fixture(golden: &Path, dir: &Path, stdout: &str, stderr: &str, tmp_body: &str) {
    use support::golden::bless;
    let ctx = dir.join("sieve");
    bless(
        &golden.join("wiring.json"),
        &fs::read(ctx.join(".graph").join("wiring.json")).expect("read actual wiring.json"),
    );
    let ask_index = ctx.join(".cache").join("ask-index.json");
    if ask_index.exists() {
        bless(
            &golden.join("ask-index.json"),
            &fs::read(&ask_index).expect("read actual ask-index.json"),
        );
    }
    let mut cards = Vec::new();
    collect_md(&ctx, Path::new(""), &mut cards);
    for card in &cards {
        bless(
            &golden.join("sieve").join(card),
            &fs::read(ctx.join(card)).expect("read actual card"),
        );
    }
    // Remove a stale card after the new ones are in place, so a parallel
    // test never sees a half-empty golden dir.
    let mut old = Vec::new();
    if golden.join("sieve").is_dir() {
        collect_md(&golden.join("sieve"), Path::new(""), &mut old);
    }
    for card in old.iter().filter(|c| !cards.contains(c)) {
        let _ = fs::remove_file(golden.join("sieve").join(card));
    }
    for name in [".gitignore", ".ignore"] {
        let target = golden
            .join(name.trim_start_matches('.'))
            .with_extension("txt");
        bless(
            &target,
            &fs::read(dir.join(name)).expect("read actual ignore file"),
        );
    }
    bless(
        &golden.join("build.stdout.txt"),
        mask_tmp(stdout, tmp_body).as_bytes(),
    );
    bless(
        &golden.join("build.stderr.txt"),
        drop_update_nudge(&mask_tmp(stderr, tmp_body)).as_bytes(),
    );
}

/// Runs `sieve build` over one fixture and checks every golden file byte
/// for byte, once, then a second time for idempotence (P2-26 to P2-32).
fn assert_fixture_matches_golden(name: &str) {
    let fixture = support::manifest_dir().join(format!("../../tests/fixtures/{name}"));
    let golden = support::manifest_dir().join(format!("../../tests/fixtures/{name}.expected"));
    let temp = TempDir::new(&format!("build-tree-{name}"));
    copy_dir(&fixture, &temp.path);

    let tmp_body = temp
        .path
        .to_str()
        .expect("utf-8 path")
        .trim_start_matches('/')
        .to_string();

    let (code, stdout, stderr) = run_build(&temp.path, &[], &[]);
    assert_eq!(code, Some(0), "{name}: exit code");
    if support::golden::blessing() {
        bless_fixture(&golden, &temp.path, &stdout, &stderr, &tmp_body);
    }

    let wiring = temp.path.join("sieve").join(".graph").join("wiring.json");
    let expected_wiring = fs::read(golden.join("wiring.json")).expect("read expected wiring.json");
    let actual_wiring = fs::read(&wiring).expect("read actual wiring.json");
    if actual_wiring != expected_wiring {
        print_diff(
            "wiring.json",
            &String::from_utf8_lossy(&actual_wiring),
            &String::from_utf8_lossy(&expected_wiring),
        );
        panic!("{name}: wiring.json mismatch");
    }

    let mut expected_cards = Vec::new();
    collect_md(&golden.join("sieve"), Path::new(""), &mut expected_cards);
    let mut actual_cards = Vec::new();
    collect_md(&temp.path.join("sieve"), Path::new(""), &mut actual_cards);
    expected_cards.sort();
    actual_cards.sort();
    assert!(
        !expected_cards.is_empty(),
        "{name}: golden card list is empty"
    );
    assert!(
        !actual_cards.is_empty(),
        "{name}: actual card list is empty"
    );
    assert_eq!(
        actual_cards, expected_cards,
        "{name}: card file set mismatch"
    );

    for card in &expected_cards {
        let expected_text =
            fs::read_to_string(golden.join("sieve").join(card)).expect("read expected card");
        let actual_text =
            fs::read_to_string(temp.path.join("sieve").join(card)).expect("read actual card");
        if actual_text != expected_text {
            print_diff(&card.display().to_string(), &actual_text, &expected_text);
            panic!("{name}: card mismatch: {}", card.display());
        }
    }

    let expected_gitignore =
        fs::read_to_string(golden.join("gitignore.txt")).expect("read gitignore.txt");
    let actual_gitignore =
        fs::read_to_string(temp.path.join(".gitignore")).expect("read actual .gitignore");
    if actual_gitignore != expected_gitignore {
        print_diff(".gitignore", &actual_gitignore, &expected_gitignore);
        panic!("{name}: .gitignore mismatch");
    }

    let expected_ignore = fs::read_to_string(golden.join("ignore.txt")).expect("read ignore.txt");
    let actual_ignore = fs::read_to_string(temp.path.join(".ignore")).expect("read actual .ignore");
    if actual_ignore != expected_ignore {
        print_diff(".ignore", &actual_ignore, &expected_ignore);
        panic!("{name}: .ignore mismatch");
    }

    let expected_stdout =
        fs::read_to_string(golden.join("build.stdout.txt")).expect("read build.stdout.txt");
    let masked_stdout = mask_tmp(&stdout, &tmp_body);
    if masked_stdout != expected_stdout {
        print_diff("build.stdout.txt", &masked_stdout, &expected_stdout);
        panic!("{name}: stdout mismatch");
    }

    let expected_stderr = drop_update_nudge(
        &fs::read_to_string(golden.join("build.stderr.txt")).expect("read build.stderr.txt"),
    );
    let masked_stderr = drop_update_nudge(&mask_tmp(&stderr, &tmp_body));
    if masked_stderr != expected_stderr {
        print_diff("build.stderr.txt", &masked_stderr, &expected_stderr);
        panic!("{name}: stderr mismatch");
    }

    // Run again: the write must stay byte-identical.
    let (code2, _, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code2, Some(0), "{name}: second run exit code");
    let second_wiring = fs::read(&wiring).expect("read wiring.json again");
    assert_eq!(
        second_wiring, expected_wiring,
        "{name}: wiring.json not idempotent"
    );
    for card in &expected_cards {
        let second_text =
            fs::read_to_string(temp.path.join("sieve").join(card)).expect("read card again");
        let expected_text =
            fs::read_to_string(golden.join("sieve").join(card)).expect("read expected card");
        assert_eq!(
            second_text,
            expected_text,
            "{name}: card not idempotent: {}",
            card.display()
        );
    }
}

#[test]
fn test_p2_26_28_29_30_31_32_build_tree_matches_golden() {
    for fixture in [
        "basic",
        "symbols",
        "scopes",
        "edges",
        "multi",
        "workspace",
        "pnpm",
    ] {
        assert_fixture_matches_golden(fixture);
    }
}

#[test]
fn test_p2_33_build_writes_a_fingerprint_with_one_print_per_claimed_file() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("build-fingerprint");
    copy_dir(&fixture, &temp.path);

    let (code, _, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code, Some(0));

    let context_dir = temp.path.join("sieve");
    let stamp = sieve_parse::extractor_stamp();
    let path = sieve_core::fingerprint::fingerprint_path(&context_dir, &stamp);
    let fingerprint = sieve_core::fingerprint::read_fingerprint(&path, &stamp)
        .expect("read fingerprint written by build");

    assert_eq!(fingerprint.version, 1);
    assert_eq!(fingerprint.extractor.len(), 16);
    assert!(fingerprint
        .extractor
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

    let claimed_files: std::collections::BTreeSet<&str> = [
        "packages/shared/index.ts",
        "py/helpers.py",
        "py/main.py",
        "src/app.ts",
        "src/legacy.mjs",
        "src/util.ts",
        "src/view.tsx",
    ]
    .into_iter()
    .collect();
    let fingerprint_keys: std::collections::BTreeSet<&str> =
        fingerprint.files.keys().map(|k| k.as_str()).collect();
    assert_eq!(fingerprint_keys, claimed_files);
}

/// Writes a TS file that declares one top-level function, so the file
/// claims exactly one node beyond its own file node.
fn write_one_function(dir: &Path, rel: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, "function f(): number {\n  return 1;\n}\n").expect("write file");
}

#[test]
fn test_p2_35_a_second_build_replays_every_file_from_cache() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("build-cache-replay");
    copy_dir(&fixture, &temp.path);

    let (code1, _, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code1, Some(0), "first build exit code");

    let wiring = temp.path.join("sieve").join(".graph").join("wiring.json");
    let index = temp.path.join("sieve").join("INDEX.md");
    let gitignore = temp.path.join(".gitignore");
    let ignore = temp.path.join(".ignore");
    let wiring_first = fs::read(&wiring).expect("read wiring.json after first build");
    let index_first = fs::read(&index).expect("read INDEX.md after first build");
    let gitignore_first = fs::read(&gitignore).expect("read .gitignore after first build");
    let ignore_first = fs::read(&ignore).expect("read .ignore after first build");

    let (code2, stdout2, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code2, Some(0), "second build exit code");
    assert!(
        stdout2.contains("parsed: 0 of 7 files (7 replayed from cache)"),
        "second build stdout: {stdout2}"
    );

    let wiring_second = fs::read(&wiring).expect("read wiring.json after second build");
    let index_second = fs::read(&index).expect("read INDEX.md after second build");
    let gitignore_second = fs::read(&gitignore).expect("read .gitignore after second build");
    let ignore_second = fs::read(&ignore).expect("read .ignore after second build");
    assert_eq!(
        wiring_first, wiring_second,
        "wiring.json not byte-identical"
    );
    assert_eq!(index_first, index_second, "INDEX.md not byte-identical");
    assert_eq!(
        gitignore_first, gitignore_second,
        ".gitignore not byte-identical"
    );
    assert_eq!(ignore_first, ignore_second, ".ignore not byte-identical");
}

#[test]
fn build_excludes_the_context_dir_and_a_prefix_sibling() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let golden = support::manifest_dir().join("../../tests/fixtures/basic.expected");
    let temp = TempDir::new("build-exclude-context-siblings");
    copy_dir(&fixture, &temp.path);

    // Plant a file under the context dir, and one under a dir whose name
    // shares the context dir's name as a string prefix. Neither should
    // reach the claimed-file list, the fingerprint, or the cards.
    write_one_function(&temp.path, "sieve/x.ts");
    write_one_function(&temp.path, "sieve-tools/y.ts");

    let tmp_body = temp
        .path
        .to_str()
        .expect("utf-8 path")
        .trim_start_matches('/')
        .to_string();
    let (code, stdout, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code, Some(0));

    let expected_stdout =
        fs::read_to_string(golden.join("build.stdout.txt")).expect("read build.stdout.txt");
    let masked_stdout = mask_tmp(&stdout, &tmp_body);
    assert_eq!(
        masked_stdout, expected_stdout,
        "stdout should match the plain basic golden: no x, no y"
    );

    let context_dir = temp.path.join("sieve");
    let stamp = sieve_parse::extractor_stamp();
    let path = sieve_core::fingerprint::fingerprint_path(&context_dir, &stamp);
    let fingerprint = sieve_core::fingerprint::read_fingerprint(&path, &stamp)
        .expect("read fingerprint written by build");
    let fingerprint_keys: std::collections::BTreeSet<&str> =
        fingerprint.files.keys().map(|k| k.as_str()).collect();
    let claimed_files: std::collections::BTreeSet<&str> = [
        "packages/shared/index.ts",
        "py/helpers.py",
        "py/main.py",
        "src/app.ts",
        "src/legacy.mjs",
        "src/util.ts",
        "src/view.tsx",
    ]
    .into_iter()
    .collect();
    assert_eq!(fingerprint_keys, claimed_files);

    let mut expected_cards = Vec::new();
    collect_md(&golden.join("sieve"), Path::new(""), &mut expected_cards);
    let mut actual_cards = Vec::new();
    collect_md(&context_dir, Path::new(""), &mut actual_cards);
    expected_cards.sort();
    actual_cards.sort();
    assert_eq!(
        actual_cards, expected_cards,
        "card set should match the plain basic golden: no x.md, no y.md"
    );
}

#[test]
fn test_p2_34_a_held_lock_makes_build_wait_then_fail() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("build-lock-held");
    copy_dir(&fixture, &temp.path);

    let cache_dir = temp.path.join("sieve").join(".cache");
    fs::create_dir_all(&cache_dir).expect("create cache dir");
    fs::write(cache_dir.join(".sync.lock"), b"{\"pid\":1,\"at\":\"now\"}")
        .expect("write a held lock file");

    let start = std::time::Instant::now();
    let (code, _, stderr) = run_build(&temp.path, &[], &[]);
    let elapsed = start.elapsed();

    assert_eq!(code, Some(1));
    assert_eq!(
        drop_update_nudge(&stderr),
        "✗ a graph rebuild is already in flight\n"
    );
    assert!(elapsed >= std::time::Duration::from_secs(2));
}

#[test]
fn build_prints_the_zero_files_report() {
    let temp = TempDir::new("build-zero-files");
    let tmp_body = temp
        .path
        .to_str()
        .expect("utf-8 path")
        .trim_start_matches('/')
        .to_string();

    let (code, stdout, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code, Some(0));

    let masked = mask_tmp(&stdout, &tmp_body);
    let expected = concat!(
        "✓ wiring: 0 nodes (), 0 edges, 0 cards []\n",
        "  parsed: 0 of 0 files (0 replayed from cache)\n",
        "  → <TMP>/sieve\n",
        "  <TMP>/sieve/ is git-ignored (added automatically) — a local cache; ",
        "teammates run `sieve build` to get their own.\n",
    );
    assert_eq!(masked, expected);
}

#[test]
fn test_p2_32_ignore_edits_are_idempotent_and_gated() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("build-idempotent-gated");
    copy_dir(&fixture, &temp.path);

    let (code, _, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code, Some(0));
    let gitignore_first =
        fs::read_to_string(temp.path.join(".gitignore")).expect("read .gitignore");
    let ignore_first = fs::read_to_string(temp.path.join(".ignore")).expect("read .ignore");

    // A second build adds nothing to either file.
    let (code, _, _) = run_build(&temp.path, &[], &[]);
    assert_eq!(code, Some(0));
    let gitignore_second =
        fs::read_to_string(temp.path.join(".gitignore")).expect("read .gitignore again");
    let ignore_second = fs::read_to_string(temp.path.join(".ignore")).expect("read .ignore again");
    assert_eq!(gitignore_first, gitignore_second);
    assert_eq!(ignore_first, ignore_second);

    // `--no-gitignore` leaves .gitignore untouched and prints the other
    // footer form.
    let no_gitignore_temp = TempDir::new("build-no-gitignore");
    copy_dir(&fixture, &no_gitignore_temp.path);
    let (code, stdout, _) = run_build(&no_gitignore_temp.path, &["--no-gitignore"], &[]);
    assert_eq!(code, Some(0));
    assert!(!no_gitignore_temp.path.join(".gitignore").exists());
    assert!(
        stdout.contains("is a local cache — add it to your gitignore if you want it untracked.")
    );
    assert!(!stdout.contains("is git-ignored (added automatically)"));
}

/// Sieve prints a `--dir` value as typed on the `→` line;
/// without `--dir` it prints the absolute `<root>/sieve`.
/// The expected lines are recorded output for the same commands. The
/// stdout golden of the build tree test sits under P2-32 (DV11 names the
/// same `--dir tools/ctx` case).
#[test]
fn test_p2_32_build_prints_the_dir_flag_as_typed_like_golden() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("build-dir-as-typed");
    let root = temp.path.join("r");
    copy_dir(&fixture, &root);
    let absolute = temp.path.join("abs");
    let absolute = absolute.to_str().expect("utf-8 path");

    for typed in ["tools/ctx", "./tools/ctx", "../up", absolute] {
        let output = support::sieve_command()
            .current_dir(&root)
            .args(["--dir", typed, "build", "."])
            .output()
            .expect("run sieve build");
        assert_eq!(output.status.code(), Some(0), "{typed}: exit code");
        let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
        let line = stdout
            .lines()
            .find(|l| l.contains('→'))
            .expect("arrow line");
        assert_eq!(line, format!("  → {typed}"), "{typed}");
    }
}
