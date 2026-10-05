//! Parity test for P1-01: the 18-row command list and one-line
//! descriptions, the `01-cli.md` note section 1.
//!
//! The Commander help renders the same text for 3 invocations:
//! `sieve` with no args (stderr, exit 1), `sieve --help` (stdout, exit
//! 0), and `sieve help` (stdout, exit 0). The golden files under
//! `tests/fixtures/basic.expected/help/` pin each one.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use support::TempDir;

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/help")
}

fn read_golden(id: &str, kind: &str) -> String {
    let path = golden_dir().join(format!("{id}.{kind}.txt"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Runs `sieve <args>` in an empty scratch dir, dropping the update
/// nudge's home dependency by pointing `$HOME` at that same empty dir —
/// no query command runs here, so no cache file is ever read or written.
fn run_sieve(cwd: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .args(args)
        .current_dir(cwd)
        .env("HOME", cwd)
        .output()
        .expect("run sieve")
}

/// Asserts one invocation's stdout, stderr, and exit code match the
/// golden byte for byte. The exit-code file holds one line, `"N\n"`.
fn assert_matches_golden(id: &str, output: &Output) {
    support::golden::bless_triple(
        &golden_dir(),
        id,
        &output.stdout,
        &output.stderr,
        output.status.code().unwrap_or(-1),
    );
    let want_stdout = read_golden(id, "stdout");
    let want_stderr = read_golden(id, "stderr");
    let want_exit: i32 = read_golden(id, "exit")
        .trim()
        .parse()
        .expect("parse exit code");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        want_stdout,
        "{id}: stdout must match sieve byte for byte"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        want_stderr,
        "{id}: stderr must match sieve byte for byte"
    );
    assert_eq!(
        output.status.code(),
        Some(want_exit),
        "{id}: exit code must match sieve"
    );
}

#[test]
fn test_p1_01_help_lists_the_18_commands_and_matches_golden() {
    let temp = TempDir::new("help-noargs");
    assert_matches_golden("noargs", &run_sieve(&temp.path, &[]));

    let temp = TempDir::new("help-dashdash");
    assert_matches_golden("dashdash", &run_sieve(&temp.path, &["--help"]));

    let temp = TempDir::new("help-help");
    assert_matches_golden("help", &run_sieve(&temp.path, &["help"]));
}

/// P1-01 follow-up: `-h`/`--help` before the first subcommand token
/// prints the full top-level help, not the subcommand's help. `-v` and
/// `--version` print the version and exit 0, matching Sieve exactly.
#[test]
fn test_p1_01_help_before_a_subcommand_and_version_flags_match_golden() {
    let temp = TempDir::new("help-dash-h-sub");
    assert_matches_golden("dash-h-sub", &run_sieve(&temp.path, &["-h", "ask"]));

    let temp = TempDir::new("help-version-long");
    assert_matches_golden("version-long", &run_sieve(&temp.path, &["--version"]));

    let temp = TempDir::new("help-version-short");
    assert_matches_golden("version-short", &run_sieve(&temp.path, &["-v"]));

    let temp = TempDir::new("help-unknown-option-then-h");
    assert_matches_golden(
        "unknown-option-then-h",
        &run_sieve(&temp.path, &["--in", "ask", "-h"]),
    );

    let temp = TempDir::new("help-operand-then-h");
    assert_matches_golden("operand-then-h", &run_sieve(&temp.path, &["foo", "-h"]));
}

/// P1-51: with no `-h` in the tail, an unknown top-level option, an
/// unknown command, or a known value flag with no value exits 1 with
/// Commander's own error line, not clap's exit 2.
#[test]
fn test_p1_51_unknown_option_and_command_match_golden() {
    let temp = TempDir::new("help-unknown-option");
    assert_matches_golden("unknown-option", &run_sieve(&temp.path, &["--bogus"]));

    let temp = TempDir::new("help-unknown-option-before-sub");
    assert_matches_golden(
        "unknown-option-before-sub",
        &run_sieve(&temp.path, &["--in", "src", "ask", "q"]),
    );

    let temp = TempDir::new("help-unknown-command");
    assert_matches_golden("unknown-command", &run_sieve(&temp.path, &["foo"]));

    let temp = TempDir::new("help-dir-missing-value");
    assert_matches_golden("dir-missing-value", &run_sieve(&temp.path, &["--dir"]));

    // P1-51 follow-up: a lone `-` is an operand, never a flag; a lone
    // `--` falls back to the no-args help; `--` followed by an operand
    // names that operand in `unknown command`.
    let temp = TempDir::new("help-lone-dash");
    assert_matches_golden("lone-dash", &run_sieve(&temp.path, &["-"]));

    let temp = TempDir::new("help-double-dash");
    assert_matches_golden("double-dash", &run_sieve(&temp.path, &["--"]));

    let temp = TempDir::new("help-double-dash-operand");
    assert_matches_golden(
        "double-dash-operand",
        &run_sieve(&temp.path, &["--", "foo"]),
    );
}

/// The commands Sieve shares with the recorded help, hidden ones included. `upgrade`,
/// `trail` and `claude-md` are absent: Sieve has none of them (P1-52, P1-53).
const SHARED_COMMANDS: &[&str] = &[
    "telemetry",
    "version",
    "build",
    "ask",
    "skeleton",
    "check",
    "stats",
    "viz",
    "mcp",
    "callers",
    "blast",
    "grep",
    "map",
    "init",
    "uninstall",
    "_brain-refresh",
    "_update-check",
    "_telemetry-flush",
];

/// The golden for one subcommand help form.
fn sub_golden(cmd: &str, form: &str) -> String {
    read_golden(&format!("sub-{cmd}-{form}"), "stdout")
}

/// Asserts one run printed the golden text on stdout, nothing on stderr,
/// exit 0.
fn assert_sub_help(what: &str, want: &str, output: &Output) {
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        want,
        "{what}: stdout"
    );
    assert!(output.stderr.is_empty(), "{what}: stderr must be empty");
    assert_eq!(output.status.code(), Some(0), "{what}: exit code");
}

/// P1-63: `<cmd> --help`, `<cmd> -h` and `help <cmd>` print the golden
/// text byte for byte, for every shared command.
#[test]
fn test_p1_63_subcommand_help_matches_golden() {
    let temp = TempDir::new("help-sub");
    for cmd in SHARED_COMMANDS {
        for (form, args) in [
            ("long", vec![*cmd, "--help"]),
            ("short", vec![*cmd, "-h"]),
            ("help", vec!["help", *cmd]),
        ] {
            let what = format!("{cmd} {form}");
            let output = run_sieve(&temp.path, &args);
            let id = format!("sub-{cmd}-{form}");
            support::golden::bless_triple(
                &golden_dir(),
                &id,
                &output.stdout,
                &output.stderr,
                output.status.code().unwrap_or(-1),
            );
            assert_sub_help(&what, &sub_golden(cmd, form), &output);
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                read_golden(&id, "stderr"),
                "{what}: stderr golden"
            );
            assert_eq!(
                output.status.code().map(|c| c.to_string()),
                Some(read_golden(&id, "exit").trim().to_string()),
                "{what}: exit golden"
            );
        }
    }
}

/// P1-63: a `-h` token after operands or an unknown option still wins,
/// as commander's `_outputHelpIfRequested` finds it.
#[test]
fn test_p1_63_help_token_after_operands_wins() {
    let temp = TempDir::new("help-sub-tail");
    let want = sub_golden("ask", "short");
    for args in [
        &["ask", "q", "-h"][..],
        &["ask", "--bogus", "-h"],
        &["ask", "q", "dir", "--help"],
    ] {
        assert_sub_help(&args.join(" "), &want, &run_sieve(&temp.path, args));
    }
}

/// P1-63: a Sieve-only command has no old name text. It keeps clap's help,
/// never the root list.
#[test]
fn test_p1_63_sieve_only_commands_keep_clap_help() {
    let temp = TempDir::new("help-sub-only");
    for cmd in ["hook", "statusline", "daemon", "savings"] {
        let output = run_sieve(&temp.path, &[cmd, "--help"]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.status.code(), Some(0), "{cmd}");
        assert!(stdout.contains(&format!("Usage: sieve {cmd}")), "{cmd}");
        assert!(
            !stdout.contains("[command]"),
            "{cmd} must not print the root help"
        );
    }
}

/// P4-47: `init --help` lists `--verbose`.
#[test]
fn test_p4_47_init_help_shows_verbose() {
    let temp = TempDir::new("help-init-verbose");
    let output = run_sieve(&temp.path, &["init", "--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            "  --verbose          print every file written, the graph build's own output and\n                     the closing banner\n"
        ),
        "{stdout}"
    );
}

/// P1-63: `help [command]` reads the root options first and ignores what
/// follows the command name. An unknown name, `help` itself, or a name
/// in another case prints the root help on stderr, exit 1. No command
/// name prints the root help on stdout.
#[test]
fn test_p1_63_help_command_follows_commander_routing() {
    let cases: &[(&str, &[&str])] = &[
        ("help-dir-x", &["--dir", "x", "help", "ask"]),
        ("help-dir-x-build", &["--dir", "x", "help", "build"]),
        ("help-ask-h", &["help", "ask", "-h"]),
        ("help-ask-json", &["help", "ask", "--json"]),
        ("help-ask-bogusflag", &["help", "ask", "--bogus"]),
        ("help-ask-q", &["help", "ask", "q"]),
        ("help-ask-build", &["help", "ask", "build"]),
        ("help-bogus", &["help", "bogus"]),
        ("help-upper", &["help", "ASK"]),
        ("help-help", &["help", "help"]),
        ("help-json", &["help", "--json"]),
    ];
    for (id, args) in cases {
        let temp = TempDir::new("help-route");
        assert_matches_golden(id, &run_sieve(&temp.path, args));
    }
}
