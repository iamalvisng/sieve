//! Parity tests for commander's error forms against a recorded run (P1-01,
//! P1-51). Each case prints one commander line on stderr and exits 1, where
//! clap prints its own text and exits 2. The query list lives in
//! `tests/inputs/cli-errors/basic.txt`. The goldens live in
//! `tests/fixtures/basic.expected/cli-errors/`.
//!
//! Each test byte-compares stdout, stderr and the exit code of the ids it
//! names.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

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

/// Masks `dir`'s realpath and literal path with `<TMP>`
/// does.
fn mask(dir: &Path, text: &str) -> String {
    let mut out = text.to_string();
    let real = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for p in [real.as_path(), dir] {
        out = out.replace(&p.display().to_string(), "<TMP>");
    }
    out
}

/// Splits a query line's arguments on whitespace. No case uses quotes.
fn split_args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// The arguments of one id in the query list.
fn listed_args(id: &str) -> Vec<String> {
    let list = support::manifest_dir().join("../../tests/inputs/cli-errors/basic.txt");
    let lines = fs::read_to_string(&list).expect("read cli-errors query list");
    let args = lines
        .lines()
        .find_map(|l| l.split_once('\t').filter(|(lid, _)| lid == &id))
        .map(|(_, args)| args)
        .unwrap_or_else(|| panic!("id {id} missing from {}", list.display()));
    split_args(args)
}

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/cli-errors")
}

/// Reads one golden file for `id`.
fn golden(id: &str, suffix: &str) -> String {
    let path = golden_dir().join(format!("{id}.{suffix}.txt"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Drops every `⬆` update-nudge line.
fn drop_nudge(s: &str) -> String {
    s.split_inclusive('\n')
        .filter(|l| !l.starts_with('⬆'))
        .collect()
}

/// Runs `sieve` with the arguments of `id` in an empty temp dir and
/// returns the ids whose stdout, stderr or exit code differ from the golden.
fn mismatches(ids: &[&str]) -> Vec<String> {
    let temp = TempDir::new("cli-errors");
    let home = TempDir::new("cli-errors-home");
    copy_dir(
        &support::manifest_dir().join("../../tests/fixtures/basic"),
        &temp.path,
    );
    let build = support::sieve_command()
        .args(["build", "."])
        .current_dir(&temp.path)
        .env("HOME", &home.path)
        .output()
        .expect("run sieve build");
    assert_eq!(build.status.code(), Some(0), "sieve build failed");
    ids.iter()
        .filter(|id| {
            let output = support::sieve_command()
                .args(listed_args(id))
                .current_dir(&temp.path)
                .env("HOME", &home.path)
                .output()
                .expect("run sieve");
            let got = (
                mask(&temp.path, &String::from_utf8_lossy(&output.stdout)),
                mask(
                    &temp.path,
                    &drop_nudge(&String::from_utf8_lossy(&output.stderr)),
                ),
                output.status.code().unwrap_or(-1),
            );
            support::golden::bless_triple(
                &golden_dir(),
                id,
                got.0.as_bytes(),
                got.1.as_bytes(),
                got.2,
            );
            let want_exit = golden(id, "exit").trim().parse::<i32>().expect("exit");
            let want = (
                golden(id, "stdout"),
                drop_nudge(&golden(id, "stderr")),
                want_exit,
            );
            if got != want {
                eprintln!("FAIL {id}\n  want {want:?}\n  got  {got:?}");
            }
            got != want
        })
        .map(|id| id.to_string())
        .collect()
}

/// P1-51 (DV6): `ask` prints commander's four error forms.
#[test]
fn test_p1_51_dv6_ask_errors() {
    let ids = [
        "ask-unknown-flag",
        "ask-unknown-far",
        "ask-missing-arg",
        "ask-missing-value",
        "ask-missing-value-in",
        "ask-extra-arg",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): `grep` prints commander's four error forms.
#[test]
fn test_p1_51_dv6_grep_errors() {
    let ids = [
        "grep-unknown-flag",
        "grep-missing-arg",
        "grep-missing-value",
        "grep-extra-arg",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): `callers` prints commander's four error forms.
#[test]
fn test_p1_51_dv6_callers_errors() {
    let ids = [
        "callers-unknown-flag",
        "callers-missing-arg",
        "callers-missing-value",
        "callers-missing-value-short",
        "callers-extra-arg",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): `check` prints commander's error forms, including the
/// variadic `-e` flags text.
#[test]
fn test_p1_51_dv6_check_errors() {
    let ids = [
        "check-unknown-flag",
        "check-missing-value",
        "check-extra-arg",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): `skeleton` prints commander's error forms, including the
/// global `--dir` with no value.
#[test]
fn test_p1_51_dv6_skeleton_errors() {
    let ids = [
        "skeleton-unknown-flag",
        "skeleton-missing-arg",
        "skeleton-missing-dir-value",
        "skeleton-extra-arg",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): `map` prints commander's error forms.
#[test]
fn test_p1_51_dv6_map_errors() {
    let ids = ["map-unknown-flag", "map-missing-value", "map-extra-arg"];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): commander reports a missing option value before an
/// unknown option, an unknown option before an extra argument, and the
/// whole typed token for an unknown option.
#[test]
fn test_p1_51_dv6_error_order_and_token() {
    let ids = [
        "order-unknown-before-extra",
        "order-missing-value-before-unknown",
        "unknown-inline-value",
        "unknown-short-group",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): a negative number is an operand or a variadic value, and
/// a short group reports its first unknown letter after a boolean flag.
#[test]
fn test_p1_51_dv6_negative_numbers_and_short_groups() {
    let ids = [
        "negative-operand",
        "negative-variadic-value",
        "short-group-unknown",
        "negative-dot-operand",
        "negative-exp-plus-operand",
        "negative-exp-both-operand",
        "negative-dot-variadic-value",
        "negative-exp-plus-variadic-value",
        "negative-exp-both-variadic-value",
        "unknown-trailing-dot",
        "unknown-upper-exp",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV6): the flags text of a missing value for `build`, `viz`,
/// `blast` and `init`, one case per value option.
#[test]
fn test_p1_51_dv6_missing_value_flags_text() {
    let ids = [
        "build-missing-value-ext",
        "build-missing-value-include-dir",
        "build-missing-value-only-dir",
        "viz-missing-value-port",
        "viz-missing-value-export",
        "viz-missing-value-title",
        "viz-missing-value-tabs",
        "blast-missing-value-base",
        "blast-missing-value-depth",
        "blast-missing-value-format",
        "blast-missing-value-export-viz",
        "blast-missing-value-title",
        "blast-missing-value-pr-author",
        "init-missing-value-agents",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-01, P1-51 (DV5): a version flag anywhere wins over help and over a
/// later fault.
#[test]
fn test_p1_51_dv5_version_forms() {
    let ids = [
        "top-vh",
        "top-version-help",
        "top-help-version",
        "top-v-version",
        "sub-after-v",
        "version-after-value-flag",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-01, P1-51 (DV5): a near-miss top-level flag gets a suggestion, a
/// known `--dir=x` leaves no operand, and a missing `--dir` value beats
/// `-h`.
#[test]
fn test_p1_51_dv5_suggestion_and_dir_forms() {
    let ids = ["top-versio", "top-dir-equals", "top-dir-missing-after-help"];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV7): an option that needs a value takes the next token even
/// when it starts with `-`, and a short flag takes an attached value.
/// The variadic flags keep the values that follow.
#[test]
fn test_p1_51_dv7_hyphen_values_and_attached_values() {
    let ids = [
        "ask-in-hyphen-value",
        "check-ext-hyphen-value",
        "check-ext-hyphen-value-then-more",
        "check-ext-attached-value",
        "blast-pr-author-hyphen-value",
        "init-agents-hyphen-value",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV7): commander lists no help option, so `-hx` and `-hh` are
/// unknown options, and a missing option value beats a later `-h`.
#[test]
fn test_p1_51_dv7_help_group_is_an_unknown_option() {
    let ids = [
        "ask-short-group-help-is-unknown",
        "ask-help-twice-is-unknown",
        "ask-help-then-missing-value",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV7): a `-h` token anywhere wins over an unknown option. Sieve
/// prints its help on stdout and exits 0.
#[test]
fn test_p1_51_dv7_help_wins_over_unknown_option() {
    let ids = [
        "ask-help-wins-over-unknown",
        "ask-unknown-group-then-help",
        "grep-flag-group-then-help",
        "check-ext-hyphen-value-then-help",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51 (DV7): the root option `--dir` after
/// a subcommand option is never that option's value. The root flags and
/// their values leave the line, and a variadic run goes on past them.
/// After `--`, every token is an operand.
#[test]
fn test_p1_51_dv7_root_flags_are_not_option_values() {
    let ids = [
        "ask-in-then-root-flag",
        "ask-in-then-root-flag-inline",
        "ask-limit-then-root-flag",
        "check-ext-then-root-flag",
        "check-ext-then-terminator-operand",
        "build-ext-then-terminator-operand",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-02, P1-51 (DV4, DV7): `-n` takes a value that starts with `-` and
/// an attached value. The value then reaches DV4: Sieve prints
/// clap's invalid-value error, exit 2. The error names
/// the value, so the value was taken and not read as a flag.
#[test]
fn test_p1_51_dv7_limit_takes_the_value_then_dv4() {
    let temp = TempDir::new("cli-limit-value");
    let cases: [(&[&str], &str); 3] = [
        (&["ask", "q", "-n", "-5"], "-5"),
        (&["ask", "q", "-nx"], "x"),
        (&["ask", "q", "-n", "-h"], "-h"),
    ];
    for (args, value) in cases {
        let output = support::sieve_command()
            .args(args)
            .current_dir(&temp.path)
            .output()
            .expect("run sieve");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("invalid value '{value}' for '--limit <n>'")),
            "{args:?}: {stderr}"
        );
        assert_eq!(output.status.code(), Some(1), "{args:?}");
    }
}

/// P4-47, P1-51: `init --dry-run` prints the plan on stderr. `-- nodir`
/// makes no difference.
#[test]
fn test_p4_47_init_dry_run_plan_matches_golden() {
    let ids = [
        "init-dry-run-default",
        "init-dry-run-claude",
        "init-dry-run-claude-terminator",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// P1-51: with `--dir x` and no graph there, `ask --in` runs and prints
/// the empty result, and `blast` and `callers` name `x` as typed.
#[test]
fn test_p1_51_dv8_missing_graph_dir_forms() {
    let ids = [
        "ask-in-with-missing-dir",
        "blast-missing-dir-relative",
        "callers-missing-dir-relative",
    ];
    assert_eq!(mismatches(&ids), Vec::<String>::new());
}

/// A flag of the old model design is not a Sieve flag. Each one exits 1 with
/// commander's unknown option line, before or after the command name.
#[test]
fn test_p1_51_removed_model_flags_are_unknown_options() {
    let temp = TempDir::new("cli-removed-flags");
    let cases: [&[&str]; 9] = [
        &["--api-key", "x", "ask", "q"],
        &["ask", "q", "--api-key", "x"],
        &["--provider", "x", "ask", "q"],
        &["--model", "x", "grep", "zz"],
        &["--base-url=x", "grep", "zz"],
        &["build", "--deep"],
        &["build", "-j", "2"],
        &["build", "--allow-partial"],
        &["blast", "--name"],
    ];
    for args in cases {
        let output = support::sieve_command()
            .args(args)
            .current_dir(&temp.path)
            .output()
            .expect("run sieve");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.starts_with("sieve: ") && stderr.contains(" option "),
            "{args:?}: {stderr}"
        );
    }
}
