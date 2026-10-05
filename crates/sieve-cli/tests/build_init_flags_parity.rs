//! Tests for the `build`, `init` and `uninstall` flags against the goldens
//! under `tests/fixtures/basic.expected/flags/` (P1-70, P1-71, P1-72,
//! P4-47, P4-48). `SIEVE_BLESS=1` writes the goldens.
//!
//! Every `init` and `uninstall` run sets `HOME` to a temp dir, and the
//! test asserts that dir is not the real home. Sieve writes under `HOME`
//! only with `--global` (ledger 2026-09-16), and no test here passes it,
//! so each init case asserts Sieve wrote nothing under `HOME`.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use support::TempDir;

fn fixture() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic")
}

fn golden_path(name: &str) -> PathBuf {
    support::manifest_dir()
        .join("../../tests/fixtures/basic.expected/flags")
        .join(name)
}

fn golden(name: &str) -> String {
    fs::read_to_string(golden_path(name)).unwrap_or_else(|e| panic!("read golden {name}: {e}"))
}

fn golden_exit(case: &str) -> i32 {
    golden(&format!("{case}.exit.txt"))
        .trim()
        .parse()
        .expect("exit code")
}

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Every file under `dir` with its bytes, keyed `./<rel>`, without
/// `sieve/.cache/` (its stamp and timestamps change on every run).
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("read dir entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).expect("strip prefix");
                let key = format!("./{}", rel.to_string_lossy().replace('\\', "/"));
                if !key.starts_with("./sieve/.cache/") {
                    out.insert(key, fs::read(&path).expect("read file"));
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The `./`-keyed paths `after` added or changed against `before`.
fn written(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(v))
        .map(|(k, _)| k.clone())
        .collect()
}

/// The `./`-keyed paths in `before` that `after` lacks.
fn removed(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    before
        .keys()
        .filter(|k| !after.contains_key(*k))
        .cloned()
        .collect()
}

/// The stderr lines that count in a compare: no `\r` progress line, no
/// update nudge.
fn stderr_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.rsplit('\r').next().unwrap_or(""))
        .filter(|l| !l.starts_with("parsing ") && !l.contains('⬆'))
        .map(str::to_string)
        .collect()
}

/// With `SIEVE_BLESS=1`, writes `rows` as the golden `name`, one row a line.
fn bless_rows(name: &str, rows: &[String]) {
    let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
    support::golden::bless_text_if_blessing(&golden_path(name), &text);
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// A scratch `HOME` that is never the real one.
fn scratch_home() -> TempDir {
    let home = TempDir::new("flags-home");
    let real = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    assert!(
        !real.as_os_str().is_empty() && !home.path.starts_with(&real),
        "scratch HOME {} sits under the real HOME {}",
        home.path.display(),
        real.display()
    );
    home
}

/// Runs `sieve <args>` in `dir` with a scratch `HOME`, and masks both
/// paths to `<TMP>`.
fn run(dir: &Path, home: &Path, args: &[&str], envs: &[(&str, &str)]) -> Run {
    let dir_real = fs::canonicalize(dir).expect("canonicalize dir");
    let home_real = fs::canonicalize(home).expect("canonicalize home");
    let mut cmd = support::sieve_command();
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", &home_real)
        .env("CLAUDE_PROJECT_DIR", &dir_real)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .env_remove("SIEVE_NO_GITIGNORE")
        .env_remove("SIEVE_NO_IGNORE");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("run sieve");
    let mask = |bytes: &[u8]| {
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        for p in [
            &dir_real,
            &home_real,
            &dir.to_path_buf(),
            &home.to_path_buf(),
        ] {
            text = text.replace(&p.display().to_string(), "<TMP>");
        }
        text
    };
    Run {
        stdout: mask(&output.stdout),
        stderr: mask(&output.stderr),
        code: output.status.code().expect("exit code"),
    }
}

/// Compares stdout, exit code and, when asked, the filtered stderr.
fn assert_case(case: &str, out: &Run, compare_stderr: bool) {
    let dir = golden_path("");
    support::golden::bless_triple(
        &dir,
        case,
        out.stdout.as_bytes(),
        out.stderr.as_bytes(),
        out.code,
    );
    assert_eq!(out.code, golden_exit(case), "{case}: exit code");
    assert_eq!(
        out.stdout,
        golden(&format!("{case}.stdout.txt")),
        "{case}: stdout"
    );
    if compare_stderr {
        assert_eq!(
            stderr_lines(&out.stderr),
            stderr_lines(&golden(&format!("{case}.stderr.txt"))),
            "{case}: stderr"
        );
    }
}

/// A fresh, unbuilt copy of the fixture.
fn fresh() -> TempDir {
    let copy = TempDir::new("flags-copy");
    copy_dir(&fixture(), &copy.path);
    copy
}

/// A copy built by `sieve build`.
fn built(home: &Path) -> TempDir {
    let copy = fresh();
    let out = run(&copy.path, home, &["build"], &[]);
    assert_eq!(out.code, 0, "sieve build failed: {}", out.stderr);
    copy
}

fn file_or_absent(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|_| "<absent>\n".to_string())
}

/// P1-70: each bad flag value exits 1 with the line, before any write.
#[test]
fn test_p1_70_build_flag_errors_match_golden() {
    let home = scratch_home();
    let cases: [(&str, &[&str]); 3] = [
        ("build-include-dir-dot", &["build", "--include-dir", ".x"]),
        ("build-include-dir-path", &["build", "--include-dir", "a/b"]),
        ("build-only-dir-empty", &["build", "--only-dir", "./"]),
    ];
    for (case, args) in cases {
        let copy = fresh();
        let out = run(&copy.path, &home.path, args, &[]);
        assert_case(case, &out, true);
        assert!(!copy.path.join("sieve").exists(), "{case}: build ran");
        assert!(!copy.path.join(".sieve").exists(), "{case}: config written");
    }
}

/// P1-70: `--include-dir dist` admits the skipped dir, persists the name
/// to `.sieve/config.json` with its `.gitignore` block, and a later plain
/// build reads the persisted override.
#[test]
fn test_p1_70_build_include_dir_admits_and_persists() {
    let home = scratch_home();
    let copy = fresh();
    let out = run(
        &copy.path,
        &home.path,
        &["build", "--include-dir", "dist"],
        &[],
    );
    assert_case("build-include-dir-dist", &out, false);
    support::golden::bless_if_blessing(
        &golden_path("build-include-dir-dist.wiring.json"),
        &fs::read(copy.path.join("sieve/.graph/wiring.json")).expect("wiring"),
    );
    support::golden::bless_optional(
        &golden_path("build-include-dir-dist.config.json"),
        Some(&file_or_absent(&copy.path.join(".sieve/config.json"))),
    );
    support::golden::bless_optional(
        &golden_path("build-include-dir-dist.gitignore.txt"),
        Some(&file_or_absent(&copy.path.join(".gitignore"))),
    );
    assert_eq!(
        fs::read(copy.path.join("sieve/.graph/wiring.json")).expect("wiring"),
        fs::read(golden_path("build-include-dir-dist.wiring.json")).expect("golden wiring"),
        "wiring.json"
    );
    assert_eq!(
        file_or_absent(&copy.path.join(".sieve/config.json")),
        golden("build-include-dir-dist.config.json")
    );
    assert_eq!(
        file_or_absent(&copy.path.join(".gitignore")),
        golden("build-include-dir-dist.gitignore.txt")
    );

    let out = run(&copy.path, &home.path, &["build"], &[]);
    assert_case("build-include-dir-persisted", &out, false);
}

/// P1-70: `--only-dir ./src/` normalizes to `src`, indexes only that
/// tree, records the whitelist in the fingerprint, and `check` honours it.
#[test]
fn test_p1_70_build_only_dir_filters_and_check_honours_it() {
    let home = scratch_home();
    let copy = fresh();
    let out = run(
        &copy.path,
        &home.path,
        &["build", "--only-dir", "./src/"],
        &[],
    );
    assert_case("build-only-dir-src", &out, false);
    support::golden::bless_if_blessing(
        &golden_path("build-only-dir-src.wiring.json"),
        &fs::read(copy.path.join("sieve/.graph/wiring.json")).expect("wiring"),
    );
    assert_eq!(
        fs::read(copy.path.join("sieve/.graph/wiring.json")).expect("wiring"),
        fs::read(golden_path("build-only-dir-src.wiring.json")).expect("golden wiring"),
        "wiring.json"
    );
    let cache = copy.path.join("sieve/.cache");
    let fingerprint = fs::read_dir(&cache)
        .expect("cache dir")
        .map(|e| e.expect("entry").path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("fingerprint."))
        })
        .expect("a fingerprint file");
    let fp: serde_json::Value =
        serde_json::from_slice(&fs::read(&fingerprint).expect("read fp")).expect("json");
    support::golden::bless_text_if_blessing(
        &golden_path("build-only-dir-src.only-dirs.txt"),
        &format!("{}\n", fp["onlyDirs"]),
    );
    assert_eq!(
        format!("{}\n", fp["onlyDirs"]),
        golden("build-only-dir-src.only-dirs.txt")
    );

    let out = run(&copy.path, &home.path, &["check"], &[]);
    assert_case("build-only-dir-src-check", &out, true);

    // An edit under `src/` forces the query refresh to rebuild; the
    // rebuild keeps the whitelist, so `py/helpers.py#add` stays out.
    let util = copy.path.join("src/util.ts");
    let mut source = fs::read(&util).expect("read util.ts");
    source.extend_from_slice(b"\n// touched\n");
    fs::write(&util, source).expect("append to util.ts");
    let out = run(&copy.path, &home.path, &["grep", "add"], &[]);
    assert_case("build-only-dir-src-grep", &out, true);
}

/// P1-70: a warm second build replays every file; `--no-reuse` parses
/// them all again.
#[test]
fn test_p1_70_build_no_reuse_parses_every_file() {
    let home = scratch_home();
    let copy = fresh();
    let out = run(&copy.path, &home.path, &["build"], &[]);
    assert_case("build-first", &out, false);
    let out = run(&copy.path, &home.path, &["build"], &[]);
    assert_case("build-second", &out, false);
    let out = run(&copy.path, &home.path, &["build", "--no-reuse"], &[]);
    assert_case("build-no-reuse", &out, false);
}

/// P1-70: a repeated `-e` adds to the list on `build` and `check`, so the
/// unknown `.zzz` still warns. The `supported:` line lists each binary's
/// own parser set, so the compare drops it.
#[test]
fn test_p1_70_repeated_extensions_accumulate() {
    let home = scratch_home();
    let copy = fresh();
    let out = run(
        &copy.path,
        &home.path,
        &["build", "-e", "zzz", "-e", "ts"],
        &[],
    );
    assert_case("build-ext-repeat", &out, false);
    assert_eq!(
        stderr_lines(&out.stderr),
        stderr_lines(&golden("build-ext-repeat.stderr.txt")),
        "build -e stderr"
    );
    let out = run(
        &copy.path,
        &home.path,
        &["check", "-e", "zzz", "-e", "ts"],
        &[],
    );
    assert_case("check-ext-repeat", &out, false);
    assert_eq!(
        stderr_lines(&out.stderr),
        stderr_lines(&golden("check-ext-repeat.stderr.txt")),
        "check -e stderr"
    );
}

/// P1-71: `--no-gitignore`, `--no-ignore` and their env vars each skip
/// one file and change the footer line.
#[test]
fn test_p1_71_no_gitignore_no_ignore_flags_and_env() {
    let home = scratch_home();
    type Case = (
        &'static str,
        &'static [&'static str],
        &'static [(&'static str, &'static str)],
    );
    let cases: [Case; 4] = [
        ("build-no-gitignore", &["build", "--no-gitignore"], &[]),
        ("build-no-ignore", &["build", "--no-ignore"], &[]),
        (
            "build-env-no-gitignore",
            &["build"],
            &[("SIEVE_NO_GITIGNORE", "1")],
        ),
        (
            "build-env-no-ignore",
            &["build"],
            &[("SIEVE_NO_IGNORE", "1")],
        ),
    ];
    for (case, args, envs) in cases {
        let copy = fresh();
        let out = run(&copy.path, &home.path, args, envs);
        assert_case(case, &out, false);
        support::golden::bless_optional(
            &golden_path(&format!("{case}.gitignore.txt")),
            Some(&file_or_absent(&copy.path.join(".gitignore"))),
        );
        support::golden::bless_optional(
            &golden_path(&format!("{case}.ignore.txt")),
            Some(&file_or_absent(&copy.path.join(".ignore"))),
        );
        assert_eq!(
            file_or_absent(&copy.path.join(".gitignore")),
            golden(&format!("{case}.gitignore.txt")),
            "{case}: .gitignore"
        );
        assert_eq!(
            file_or_absent(&copy.path.join(".ignore")),
            golden(&format!("{case}.ignore.txt")),
            "{case}: .ignore"
        );
    }
}

/// Runs one init case on `copy` and compares its report and write set.
fn assert_init_case(case: &str, copy: &Path, args: &[&str], compare_stderr: bool) {
    let home = scratch_home();
    let before = snapshot(copy);
    let out = run(copy, &home.path, args, &[]);
    assert_case(case, &out, compare_stderr);
    let after = snapshot(copy);
    let wrote = written(&before, &after);
    if support::golden::blessing() {
        bless_rows(&format!("{case}.written.txt"), &wrote);
        let _ = fs::remove_dir_all(golden_path(&format!("{case}.files")));
        for rel in &wrote {
            support::golden::bless(
                &golden_path(&format!("{case}.files/{rel}")),
                after.get(rel).expect("written file"),
            );
        }
    }
    let want: Vec<String> = golden(&format!("{case}.written.txt"))
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(wrote, want, "{case}: write set");
    for rel in &wrote {
        let expected = fs::read(golden_path(&format!("{case}.files/{rel}"))).expect("golden file");
        assert_eq!(
            after.get(rel).expect("written file"),
            &expected,
            "{case}: {rel} bytes"
        );
    }
    assert!(
        snapshot(&home.path).is_empty(),
        "{case}: wrote under HOME without --global"
    );
}

/// P4-47: each init flag gates its write set. `--dry-run`
/// writes nothing; its plan text is pinned in commander_errors_parity, so only the write set,
/// stdout and exit code count there.
#[test]
fn test_p4_47_init_flags_gate_the_write_set() {
    let home = scratch_home();
    let copy = built(&home.path);
    assert_init_case(
        "init-list-agents",
        &copy.path,
        &["init", "--list-agents"],
        true,
    );
    let copy = fresh();
    assert_init_case(
        "init-no-build",
        &copy.path,
        &["init", "--no-agents", "--no-build"],
        true,
    );
    let copy = built(&home.path);
    assert_init_case("init-no-agents", &copy.path, &["init", "--no-agents"], true);
    let copy = built(&home.path);
    assert_init_case(
        "init-no-mcp",
        &copy.path,
        &["init", "--all-agents", "--no-mcp"],
        true,
    );
    let copy = built(&home.path);
    assert_init_case(
        "init-no-hooks",
        &copy.path,
        &["init", "--all-agents", "--no-hooks"],
        true,
    );
    let copy = built(&home.path);
    assert_init_case(
        "init-dry-run",
        &copy.path,
        &["init", "--dry-run", "--all-agents"],
        false,
    );
    let copy = built(&home.path);
    assert_init_case("init-yes", &copy.path, &["init", "-y"], true);
}

/// P4-48: `--no-global` writes nothing outside the repo and prints
/// The skip line.
#[test]
fn test_p4_48_init_no_global_skips_home() {
    let home = scratch_home();
    let copy = built(&home.path);
    assert_init_case(
        "init-no-global",
        &copy.path,
        &["init", "--all-agents", "--no-global"],
        true,
    );
    assert!(golden("init-no-global.written.txt")
        .lines()
        .all(|l| !l.starts_with("home/")));
}

/// P1-72: `uninstall` without `-y` removes nothing; `-y` removes the
/// wiring and the cache; `--keep-cache` leaves `sieve/` and `.gitignore`;
/// `--no-global` removes nothing outside the repo.
#[test]
fn test_p1_72_uninstall_flags_match_golden() {
    let cases: [(&str, &[&str]); 4] = [
        ("uninstall-dry", &["uninstall"]),
        ("uninstall-yes", &["uninstall", "-y"]),
        ("uninstall-keep-cache", &["uninstall", "-y", "--keep-cache"]),
        ("uninstall-no-global", &["uninstall", "-y", "--no-global"]),
    ];
    for (case, args) in cases {
        let home = scratch_home();
        let copy = built(&home.path);
        let out = run(&copy.path, &home.path, &["init", "--all-agents"], &[]);
        assert_eq!(out.code, 0, "{case}: init failed: {}", out.stderr);
        let before = snapshot(&copy.path);
        let out = run(&copy.path, &home.path, args, &[]);
        assert_case(case, &out, false);
        let after = snapshot(&copy.path);
        let gone = removed(&before, &after);
        bless_rows(&format!("{case}.removed.txt"), &gone);
        let want: Vec<String> = golden(&format!("{case}.removed.txt"))
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(gone, want, "{case}: removed set");
        assert_eq!(
            copy.path.join("sieve").is_dir(),
            case == "uninstall-dry" || case == "uninstall-keep-cache",
            "{case}: sieve/ presence"
        );
        assert!(snapshot(&home.path).is_empty(), "{case}: touched HOME");
        // The dry-run line is the one shared report line.
        assert_eq!(
            out.stderr
                .contains("Dry run — nothing was touched. Re-run with -y to remove."),
            case == "uninstall-dry",
            "{case}: dry-run line"
        );
        // `--no-global` drops Sieve's own `--global` hint (P1-72).
        if case == "uninstall-no-global" {
            assert!(
                !out.stderr.contains("pass --global"),
                "{case}: hint printed"
            );
        }
    }
}
