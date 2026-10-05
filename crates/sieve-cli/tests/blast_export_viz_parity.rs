//! Parity tests for `sieve blast --export-viz <dir>` (P1-24, P1-65), against
//! the goldens under `tests/fixtures/edges.expected/blast-viz/`. Each case is
//! one `tests/inputs/blast-viz/<id>.sh` in the `blast-fmt` shape. Each golden
//! holds the stdout, stderr, the exit code and the written `index.html`.
//!
//! The copy sits at `<temp>/edges`, so the page's `repoName` is `edges` on both
//! sides. The owner case pins its author date, so the owner `last` day in the
//! page is byte-stable.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use support::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("read file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Drops every `⬆` update-nudge line, then rejoins with one trailing
/// newline when anything remains.
fn normalize_stderr(s: &str) -> String {
    let kept: Vec<&str> = s.lines().filter(|l| !l.starts_with('⬆')).collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

/// Replaces every spelling of `root` with `<TMP>`.
fn mask_tmp(s: &str, root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    s.replace(&canon.display().to_string(), "<TMP>")
        .replace(&root.display().to_string(), "<TMP>")
}

/// Runs git in `root`'s fixture identity. `blast` drops
/// the repo-local identity from its owners, so the base commit must
/// carry that identity.
fn run_git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "sieve-fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@sieve.local")
        .env("GIT_COMMITTER_NAME", "sieve-fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@sieve.local")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn case_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/inputs/blast-viz")
}

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/edges.expected/blast-viz")
}

/// Builds the `edges` fixture into `<temp>/edges`, commits the built
/// tree, applies the two standard edits, then runs the case setup script
/// in the copy. Returns the temp dir and the case's arguments.
fn prepare_case(id: &str) -> (TempDir, Vec<String>) {
    let fixture = support::manifest_dir().join("../../tests/fixtures/edges");
    let temp = TempDir::new(&format!("blast-viz-{id}"));
    let root = temp.path.join("edges");
    copy_dir(&fixture, &root);

    let output = support::sieve_command()
        .args(["build", "."])
        .current_dir(&root)
        .output()
        .expect("run sieve build");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    run_git(&root, &["init", "-q"]);
    run_git(&root, &["config", "user.name", "sieve-fixture"]);
    run_git(&root, &["config", "user.email", "fixture@sieve.local"]);
    run_git(&root, &["add", "-A"]);
    run_git(&root, &["commit", "-q", "-m", "base"]);

    let derived = root.join("ts").join("derived.ts");
    let mut source = fs::read_to_string(&derived).expect("read derived.ts");
    source.push_str("\nexport function extra(): number { return greet().length; }\n");
    fs::write(&derived, source).expect("edit derived.ts");
    fs::write(
        root.join("ts").join("newfile.ts"),
        "export function newFn(): number {\n  return 1;\n}\n",
    )
    .expect("write newfile.ts");
    run_git(&root, &["add", "-A"]);

    // The case list runs `edges.blast.txt` in the same copy before it
    // captures the cases, so the graph is fresh over the two edits when
    // a case setup starts. One discarded query mirrors that.
    let warm = support::sieve_command()
        .args(["blast", "-d", "1"])
        .current_dir(&root)
        .output()
        .expect("run warm-up blast");
    assert_eq!(warm.status.code(), Some(0), "warm-up blast failed");

    let script = case_dir().join(format!("{id}.sh"));
    let text = fs::read_to_string(&script).unwrap_or_else(|e| panic!("read {id}.sh: {e}"));
    let args = text
        .lines()
        .find_map(|l| l.strip_prefix("# args: "))
        .unwrap_or_else(|| panic!("{id}.sh has no `# args:` line"));
    let argv: Vec<String> = args.split_whitespace().map(str::to_string).collect();

    // No identity in the environment: a case commits as "Other Person"
    // through `-c`, and an environment identity would override that. A
    // plain commit in a case reads the repo config set above.
    let status = Command::new("bash")
        .arg(&script)
        .current_dir(&root)
        .status()
        .expect("run case setup");
    assert!(status.success(), "setup {id}.sh failed");
    (temp, argv)
}

/// Replaces the `last 30d ago` tail of a `who to tag` line with
/// `last <AGE>`: the owner case pins its author date, so the text report
/// ages with the clock while the page's `last` day does not.
fn mask_age(s: &str) -> String {
    s.lines()
        .map(|line| match line.find(", last ") {
            Some(at) => format!("{}, last <AGE>", &line[..at]),
            None => line.to_string(),
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Reads one golden capture for `id`.
fn golden(id: &str, suffix: &str) -> String {
    fs::read_to_string(golden_dir().join(format!("{id}.{suffix}")))
        .unwrap_or_else(|e| panic!("read golden blast-viz/{id}.{suffix}: {e}"))
}

/// Runs one case, then compares stdout, stderr, the exit code and the
/// `index.html` bytes with the goldens.
fn assert_case_matches_golden(id: &str) {
    let (temp, argv) = prepare_case(id);
    let root = temp.path.join("edges");
    let mut cmd = support::sieve_command();
    let output = cmd
        .args(&argv)
        .current_dir(&root)
        .output()
        .expect("run sieve blast");
    let stdout = mask_tmp(&String::from_utf8_lossy(&output.stdout), &temp.path);
    let stderr = normalize_stderr(&mask_tmp(
        &String::from_utf8_lossy(&output.stderr),
        &temp.path,
    ));
    support::golden::bless_triple(
        &golden_dir(),
        id,
        stdout.as_bytes(),
        stderr.as_bytes(),
        output.status.code().unwrap_or(-1),
    );
    if support::golden::blessing() {
        let page = fs::read(root.join("vizout").join("index.html")).expect("read page");
        support::golden::bless(&golden_dir().join(format!("{id}.index.html")), &page);
    }
    assert_eq!(
        support::mask_kb(&mask_age(&stdout)),
        support::mask_kb(&mask_age(&golden(id, "stdout.txt"))),
        "blast-viz {id}: stdout"
    );
    assert_eq!(
        support::mask_kb(&stderr),
        support::mask_kb(&normalize_stderr(&golden(id, "stderr.txt"))),
        "blast-viz {id}: stderr"
    );
    assert_eq!(
        output.status.code().unwrap_or(-1).to_string(),
        golden(id, "exit.txt").trim(),
        "blast-viz {id}: exit"
    );

    // The viewer is original code, so only the data block must match.
    let want = support::data_block(
        &fs::read(golden_dir().join(format!("{id}.index.html")))
            .unwrap_or_else(|e| panic!("read golden blast-viz/{id}.index.html: {e}")),
    )
    .into_bytes();
    let written = root.join("vizout").join("index.html");
    let got = support::data_block(
        &fs::read(&written).unwrap_or_else(|e| panic!("read {}: {e}", written.display())),
    )
    .into_bytes();
    if want != got {
        let (w, g) = (
            String::from_utf8_lossy(&want),
            String::from_utf8_lossy(&got),
        );
        let at = w
            .char_indices()
            .zip(g.chars())
            .find(|((_, a), b)| a != b)
            .map(|((i, _), _)| i)
            .unwrap_or(w.len().min(g.len()));
        let lo = at.saturating_sub(160);
        panic!(
            "blast-viz {id}: index.html differs at byte {at} (want {} bytes, got {} bytes)\nwant: {:?}\ngot:  {:?}",
            want.len(),
            got.len(),
            &w[lo..(at + 160).min(w.len())],
            &g[lo..(at + 160).min(g.len())]
        );
    }
}

/// P1-24, P1-65: the standard edits at depth 1 export one `changed:`
/// node, no edge, and print the `--export-viz:` note on stderr before
/// the text report.
#[test]
fn test_p1_24_p1_65_export_viz_d1_one_area() {
    assert_case_matches_golden("d1");
}

/// P1-24, P1-65: two areas and two modules export `changed:` and
/// `affected:` nodes with evidence and owners, `depends_on` edges, and
/// the `--title` subtitle.
#[test]
fn test_p1_24_p1_65_export_viz_two_areas_and_two_modules() {
    assert_case_matches_golden("modules");
}

/// P1-24, P1-65: an `origin` remote names the page after the remote's
/// last path segment, not the directory.
#[test]
fn test_p1_24_p1_65_export_viz_repo_name_from_origin() {
    assert_case_matches_golden("origin");
}

/// P1-24, P1-65: a diff no parser claims exports an empty graph with the
/// "no parser claims" empty note.
#[test]
fn test_p1_24_p1_65_export_viz_unindexed_diff_has_no_nodes() {
    assert_case_matches_golden("unindexed");
}
