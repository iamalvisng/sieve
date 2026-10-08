//! The `sieve` command-line binary. Each subcommand is a stub until its
//! crate gains real logic. `build` writes the graph tree under `sieve/`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

mod ask;
mod blast;
mod build;
mod callers;
mod check;
mod commander;
mod grep;
mod help;
mod hook;
mod hosts;
mod init;
mod intercept;
mod internal;
mod jsonv;
mod map;
mod mcp;
mod names;
mod ojson;
mod pane_mod;
mod query;
mod route;
mod skeleton;
mod stats;
mod statusline;
mod telemetry;
mod templates;
mod ui;
mod uninstall;
mod version;
mod viz;
mod viz_serve;
mod why;
mod why_rows;

/// The `--dir` flag's help text, branded with the active product's name
/// (Task B: clap's derived `--help` reaches this text, so it must never
/// hard-code `sieve`).
fn dir_help() -> String {
    format!(
        "The context dir. Default: `<dir>/{}`.",
        sieve_core::product().name
    )
}

/// The `--dir` flag's long help text: the field-naming rationale below is
/// product-name-free, so it needs no branding, only its own function —
/// clap only auto-fills `long_help` from a doc comment when neither
/// `help` nor `long_help` is set explicitly.
fn dir_long_help() -> String {
    format!(
        "{}\n\n\
         This field is named `context_dir`, not `dir`, on purpose: clap \
         derives an arg's id from its field name, and a global arg shares \
         its id with any subcommand arg of the same name. Every subcommand \
         below also has its own `dir` field (the repo-root positional), so \
         naming this field `dir` made clap fill both fields from one \
         `--dir` value — the repo root then silently became the context \
         dir. The flag itself still reads `--dir` on the command line.",
        dir_help()
    )
}

/// Commander takes the
/// next token as a required option's value, even `-1`, and lets a repeated
/// flag win with its last value. `args_override_self` ports the second
/// rule for every subcommand; each single-value flag carries
/// `allow_hyphen_values` for the first.
#[derive(Parser)]
#[command(name = "sieve", args_override_self = true)]
struct Cli {
    #[arg(
        long = "dir",
        global = true,
        allow_hyphen_values = true,
        value_name = "path",
        help = dir_help(),
        long_help = dir_long_help()
    )]
    context_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

/// The `build` subcommand's help text, branded with the active product's
/// name (Task B: reaches `sieve build --help` through clap's derived
/// help).
fn build_about() -> String {
    format!(
        "Builds the graph for a repo and writes the `{}/` tree.",
        sieve_core::product().name
    )
}

#[derive(Subcommand)]
enum Command {
    #[command(about = build_about())]
    Build(build::BuildArgs),
    /// Queries the indexed graph for ranked hits.
    Ask(ask::AskArgs),
    /// Searches the indexed graph for a pattern.
    Grep(grep::GrepArgs),
    /// Summarizes one file's definitions, signatures only.
    Skeleton(skeleton::SkeletonArgs),
    /// Walks who calls, references, implements or extends a symbol.
    Callers(callers::CallersArgs),
    /// Reports what a diff's changed lines reach.
    Blast(blast::BlastArgs),
    /// Reports a deterministic, token-budgeted repo orientation.
    Map(map::MapArgs),
    /// Checks whether the committed graph still matches the code.
    Check(check::CheckArgs),
    /// Serves the MCP tools over stdio, one NDJSON request per line.
    Mcp(mcp::McpArgs),
    /// Answers one Claude Code hook event on stdin, emits its JSON reply.
    Hook(hook::HookArgs),
    /// Renders the Claude Code statusline from the canned status JSON on stdin.
    Statusline(statusline::StatuslineArgs),
    /// Wires this repo (and `$HOME`) into every detected or selected agent host.
    Init(init::InitArgs),
    /// Removes every file `sieve init` wrote.
    Uninstall(uninstall::UninstallArgs),
    /// Prints the installed version and the latest cached on npm.
    Version(version::VersionArgs),
    /// Shows this agent session's sieve-vs-source usage mix and tokens saved.
    Stats(stats::StatsArgs),
    /// Shows, inspects, or turns off the anonymous usage stats.
    Telemetry(telemetry::TelemetryArgs),
    /// Exports the interactive graph viewer as one self-contained page.
    Viz(viz::VizArgs),
    /// Hidden. Reserved for the update cache refresh; Sieve does nothing.
    #[command(name = "_update-check", hide = true)]
    UpdateCheck,
    /// Hidden. Reserved for the telemetry queue post; Sieve does nothing.
    #[command(name = "_telemetry-flush", hide = true)]
    TelemetryFlush,
    /// Hidden. Reserved for the brain rules pull; Sieve does nothing.
    #[command(name = "_brain-refresh", hide = true)]
    BrainRefresh(internal::BrainRefreshArgs),
    /// Shows the decision doc entries that explain a symbol (Sieve only).
    Why(why::WhyArgs),
    Daemon,
    Savings,
}

/// Every subcommand name Commander registers on `program` in the
/// installed 0.20.0 (`grep -n '\.command('`), hidden ones
/// included: `_brain-refresh`, `_update-check` and `_telemetry-flush`
/// are hidden from the rendered help but still claim `operands[0]`, so a
/// `-h` after one of them is that hidden subcommand's own help, not
/// top-level help. Sieve does not implement every name here (`upgrade`,
/// `viz`); they still belong in this list because the list's job is only
/// to answer "does Commander treat this token as a subcommand", not
/// "does Sieve implement it".
const SUBCOMMAND_NAMES: &[&str] = &[
    "_brain-refresh",
    "_update-check",
    "_telemetry-flush",
    "telemetry",
    "version",
    "upgrade",
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
    "hook",
    "statusline",
    "daemon",
    "savings",
];

/// True when Commander registers `arg` as a subcommand.
fn is_subcommand(arg: &str) -> bool {
    SUBCOMMAND_NAMES.contains(&arg) || arg == "why"
}

/// The top-level flags Commander's own `program` registers that take a
/// value ('s `.option(...)` calls above the `.command(...)`
/// list). Commander's option scan recognizes these anywhere in the
/// top-level args, whether or not an earlier unrecognized option already
/// switched the scan into "unknown" mode.
const VALUE_FLAGS: &[&str] = &["--dir"];

/// True when `-h`/`--help` lands as Commander's own top-level help,
/// never a subcommand's.
///
/// Commander's option scanner walks the raw args left to right. A token
/// it does not recognize as one of its own top-level options (`-h` and
/// `--help` are never in that set — Commander checks for them
/// separately, after the scan) switches the scan into "unknown" mode;
/// every later token, subcommand names included, then counts as
/// "unknown" too, because the scan itself no longer tries to route to a
/// subcommand. Once the scan ends, Commander dispatches to a subcommand
/// only when the *first* token still classified as a plain positional
/// (never switched to "unknown") is a registered subcommand name; every
/// other case — no subcommand claimed, or an unrecognized option
/// preceded the name — falls back to printing the top-level help if
/// `-h`/`--help` shows up anywhere in the unknown tail (`sieve -h ask`,
/// `sieve --in ask -h`, `sieve foo -h` all hit this fallback).
fn help_precedes_subcommand(args: &[String]) -> bool {
    let mut unknown_mode = false;
    let mut dispatched = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if VALUE_FLAGS.contains(&arg) {
            i += 2;
            continue;
        }
        if arg == "-v" || arg == "--version" || commander::is_inline_value_flag(arg) {
            i += 1;
            continue;
        }
        if arg == "-h" || arg == "--help" {
            return !dispatched;
        }
        if !unknown_mode && !dispatched {
            if is_subcommand(arg) {
                dispatched = true;
            } else if arg.starts_with('-') {
                unknown_mode = true;
            }
        }
        i += 1;
    }
    false
}

/// The names `help <name>` leaves to the old path: the Sieve-only
/// commands, and the `upgrade`, `trail` and `claude-md`, which Sieve
/// lacks by design (P1-52, P1-53).
const HELP_KEEPS_CLAP: &[&str] = &[
    "hook",
    "statusline",
    "daemon",
    "savings",
    "upgrade",
    "trail",
    "claude-md",
];

/// P1-63: ports commander's root operand scan for the `help` command
/// (`parseOptions`, `_dispatchHelpCommand`). Root options are read
/// anywhere up to the first unknown option, which ends the operands.
/// Returns `None` when the first operand is not `help`. Otherwise it
/// returns the second operand, if any.
fn help_target(args: &[String]) -> Option<Option<&str>> {
    let mut operands: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if arg == "--" {
            operands.extend(args[i..].iter().map(String::as_str));
            break;
        }
        if VALUE_FLAGS.contains(&arg) {
            i += 1;
        } else if commander::is_inline_value_flag(arg) {
            continue;
        } else if arg.starts_with('-') && arg != "-" {
            break;
        } else {
            operands.push(arg);
        }
    }
    (operands.first() == Some(&"help")).then(|| operands.get(1).copied())
}

/// P1-51: Commander's top-level scan error, when the args need no help
/// text (the caller already ruled out [`help_precedes_subcommand`]).
///
/// Commander's option scan (see [`help_precedes_subcommand`]) also stops
/// at the first fault it meets, left to right: a known value flag with no
/// following token exits during the scan itself
/// (`optionMissingArgument`), ahead of any subcommand check. Past that, a
/// flag the scan does not recognize, met before any subcommand name
/// claims its slot, is `unknownOption`. A plain word in that same slot
/// that is not a registered subcommand name (nor `help`, Commander's own
/// built-in subcommand) is `unknownCommand`. Once a subcommand name
/// claims the slot, Commander hands the rest of the args to that
/// subcommand, so this function reports nothing past that point — clap
/// takes over.
///
/// Two tokens never count as a flag, even though they start with `-`:
/// a lone `-` is an operand (`error: unknown command '-'`), and `--` is
/// the option-scan terminator. Every token after `--` is an operand too,
/// whatever its own spelling, so `sieve -- foo` reports `unknown command
/// 'foo'`, not `unknown option`. (A lone `--`, with no operand after it,
/// never reaches this function; `main` handles that case up front.)
fn preflight_error(args: &[String]) -> Option<String> {
    let mut dispatched = false;
    let mut past_terminator = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if !past_terminator && arg == "--" {
            past_terminator = true;
            i += 1;
            continue;
        }
        if !past_terminator {
            if let Some((_, flags)) = commander::VALUE_FLAG_TEXT
                .iter()
                .find(|(flag, _)| *flag == arg)
            {
                if i + 1 >= args.len() {
                    return Some(format!("error: option '{flags}' argument missing"));
                }
                i += 2;
                continue;
            }
            if arg == "-v" || arg == "--version" || commander::is_inline_value_flag(arg) {
                i += 1;
                continue;
            }
        }
        if !dispatched {
            if is_subcommand(arg) || arg == "help" {
                dispatched = true;
            } else if !past_terminator && arg.starts_with('-') && arg != "-" {
                return Some(commander::unknown_option_root(arg));
            } else {
                return Some(format!("error: unknown command '{arg}'"));
            }
        }
        i += 1;
    }
    None
}

fn main() -> ExitCode {
    // P1-01: the Commander help renders the same 18-row command list
    // for `sieve` (no args, stderr, exit 1), `sieve --help`, and `sieve
    // help` (stdout, exit 0 for both). Clap's own derived help has a
    // different layout, so sieve intercepts these top-level forms before
    // clap parses, ahead of any subcommand. `--version`/`-v` get the same
    // early intercept (Commander prints the version and exits before any
    // subcommand routing runs).
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    // P1-51: Commander's top-level scan runs over every token first. A
    // version flag anywhere prints the version. A top-level value flag
    // with no value is an error. If every token was a known option, no
    // operand is left, and Commander prints the help on stderr, exit 1.
    match commander::scan_top_level(&raw_args) {
        Some(commander::TopScan::Version) => {
            println!("{}", version::current_version());
            return ExitCode::SUCCESS;
        }
        Some(commander::TopScan::MissingValue(flags)) => {
            ui::print_commander_error(&format!("error: option '{flags}' argument missing"));
            return ExitCode::FAILURE;
        }
        Some(commander::TopScan::NoOperands) => {
            eprint!("{}", help::help_text());
            return ExitCode::FAILURE;
        }
        None => {}
    }
    // P1-63: commander's `help [command]`, after the root options.
    if let Some(target) = help_target(&raw_args) {
        match target {
            None => {
                print!("{}", help::help_text());
                return ExitCode::SUCCESS;
            }
            Some(name) => {
                if let Some(text) = help::sub_help_text(name) {
                    print!("{text}");
                    return ExitCode::SUCCESS;
                }
                if !HELP_KEEPS_CLAP.contains(&name) && !is_subcommand(name) {
                    // An unknown name: commander dispatches it and prints
                    // the root help on stderr.
                    eprint!("{}", help::help_text());
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    match raw_args.as_slice() {
        [] => {
            eprint!("{}", help::help_text());
            return ExitCode::FAILURE;
        }
        [only] if only == "help" => {
            print!("{}", help::help_text());
            return ExitCode::SUCCESS;
        }
        [only] if only == "--version" || only == "-v" => {
            println!("{}", version::current_version());
            return ExitCode::SUCCESS;
        }
        [only] if only == "--" => {
            // P1-51 follow-up: a lone `--` ends Commander's option scan
            // with zero operands left. Commander then falls back to the
            // same no-args behavior: the full help text on stderr, exit
            // 1.
            eprint!("{}", help::help_text());
            return ExitCode::FAILURE;
        }
        _ if help_precedes_subcommand(&raw_args) => {
            print!("{}", help::help_text());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // P1-51: an unknown top-level option, an unknown command, or a known
    // value flag with no value exits 1 with Commander's own line, ahead
    // of clap's parse (clap's exit 2 and message differ from Commander's).
    if let Some(message) = preflight_error(&raw_args) {
        ui::print_commander_error(&message);
        return ExitCode::FAILURE;
    }

    // P1-01, P1-51: a clap error with a commander form prints that one
    // line and exits 1. Any other clap error keeps clap's own output.
    // Commander reads a negative number as an operand in a leaf command
    // (`negativeNumberArg`). Clap's number test differs
    // from commander's regex, so `rewrite_negative_numbers` rewrites the
    // argv from commander's scan. `precheck` ports commander's order for a
    // missing value, `-h` and an unknown option.
    let mut root = Cli::command();
    root.build();
    let program = std::env::args().next().unwrap_or_else(|| "sieve".into());
    let precheck = commander::precheck(&root, &raw_args);
    // P1-63: `<cmd> -h` and `<cmd> --help` print the text when one exists.
    // Clap's help stays for the Sieve-only commands.
    if let Some(commander::Precheck::Help(argv)) = &precheck {
        let name = argv.iter().rev().nth(1).map_or("", String::as_str);
        if let Some(text) = help::sub_help_text(name) {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
    }
    if let Some(commander::Precheck::Line(line)) = &precheck {
        ui::print_commander_error(line);
        return ExitCode::FAILURE;
    }
    let rewritten = match precheck {
        Some(commander::Precheck::Help(argv)) => Some(argv),
        _ => commander::rewrite_negative_numbers(&root, &raw_args),
    };
    let argv: Vec<String> = match rewritten {
        Some(rewritten) => std::iter::once(program).chain(rewritten).collect(),
        None => std::iter::once(program).chain(raw_args.clone()).collect(),
    };
    let parsed = root.clone().try_get_matches_from(argv).and_then(|matches| {
        Cli::from_arg_matches(&matches).map_err(|err| err.format(&mut root.clone()))
    });
    let cli = match parsed {
        Ok(cli) => cli,
        Err(err) => {
            if let Some(line) = commander::line_for(&err, &root, &raw_args) {
                ui::print_commander_error(&line);
                return ExitCode::FAILURE;
            }
            if !err.use_stderr() {
                err.exit()
            }
            // Any other clap error gets the same one-line shape, exit 1.
            let text = err.to_string();
            let first = text.lines().next().unwrap_or("");
            ui::print_commander_error(first);
            return ExitCode::FAILURE;
        }
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            ui::print_error(&message);
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Build(args) => build::run(&args, cli.context_dir.as_deref()),
        Command::Ask(args) => ask::run(&args, cli.context_dir.as_deref()),
        Command::Grep(args) => grep::run(&args, cli.context_dir.as_deref()),
        Command::Skeleton(args) => skeleton::run(&args, cli.context_dir.as_deref()),
        Command::Callers(args) => callers::run(&args, cli.context_dir.as_deref()),
        Command::Blast(args) => blast::run(&args, cli.context_dir.as_deref()),
        Command::Map(args) => map::run(&args, cli.context_dir.as_deref()),
        Command::Check(args) => check::run(&args, cli.context_dir.as_deref()),
        Command::Mcp(args) => mcp::run(&args, cli.context_dir.as_deref()),
        Command::Hook(args) => hook::run(&args, cli.context_dir.as_deref()),
        Command::Statusline(args) => statusline::run(&args, cli.context_dir.as_deref()),
        Command::Init(args) => init::run(&args, cli.context_dir.as_deref()),
        Command::Uninstall(args) => uninstall::run(&args, cli.context_dir.as_deref()),
        Command::Version(args) => version::run(&args),
        Command::Stats(args) => stats::run(&args, cli.context_dir.as_deref()),
        Command::Telemetry(args) => telemetry::run(&args),
        Command::Viz(args) => viz::run(&args, cli.context_dir.as_deref()),
        Command::UpdateCheck => internal::update_check(),
        Command::TelemetryFlush => internal::telemetry_flush(),
        Command::BrainRefresh(args) => internal::brain_refresh(&args),
        Command::Why(args) => why::run(&args, cli.context_dir.as_deref()),
        Command::Daemon => {
            println!("not implemented");
            Ok(())
        }
        Command::Savings => {
            println!("not implemented");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Cli {
        Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("parse {argv:?}: {e}"))
    }

    /// The `--long` flags in the Options section of one help text.
    fn text_flags(text: &str) -> Vec<String> {
        let mut flags = Vec::new();
        let options = text.split("Options:\n").nth(1).unwrap_or("");
        for line in options.lines().filter(|l| l.starts_with("  -")) {
            let column = line.split("  ").nth(1).unwrap_or("");
            flags.extend(
                column
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .filter(|w| w.starts_with("--") && *w != "--help")
                    .map(str::to_string),
            );
        }
        flags
    }

    /// P1-63: each text in `SUB_HELP` lists the long flags of its clap
    /// command, and no more.
    #[test]
    fn test_p1_63_help_text_flags_match_the_clap_flags() {
        let mut root = Cli::command();
        root.build();
        for (name, text) in help::SUB_HELP {
            let sub = root.find_subcommand(name).expect(name);
            let mut clap_flags: Vec<String> = sub
                .get_arguments()
                .filter(|a| !a.is_global_set())
                .filter_map(|a| a.get_long())
                // `--global` on `init` and `uninstall` is a Sieve-only flag
                // (ledger 2026-09-16); the text does not list it.
                .filter(|l| *l != "help" && *l != "global" && *l != "mod")
                .map(|l| format!("--{l}"))
                .collect();
            let mut in_text = text_flags(text);
            clap_flags.sort();
            in_text.sort();
            assert_eq!(in_text, clap_flags, "{name}");
        }
    }

    /// Commander takes `-1` as a value and lets the last repeat win
    /// (the commander rule); the parse must reach Sieve's own
    /// check with the text, not a clap error.
    #[test]
    fn test_p1_23_clap_takes_hyphen_values_and_last_repeat() {
        match parse(&["sieve", "callers", "add", "--depth", "-1"]).command {
            Command::Callers(a) => assert_eq!(a.depth, "-1"),
            _ => panic!("not callers"),
        }
        let argv = [
            "sieve", "callers", "add", "--depth", "2", "-d", "3", "--json", "--json",
        ];
        match parse(&argv).command {
            Command::Callers(a) => {
                assert_eq!(a.depth, "3");
                assert!(a.json);
            }
            _ => panic!("not callers"),
        }
        match parse(&["sieve", "map", "--max-dirs", "1", "--max-dirs", "-1"]).command {
            Command::Map(a) => assert_eq!(a.max_dirs.as_deref(), Some("-1")),
            _ => panic!("not map"),
        }
        let argv = ["sieve", "build", "--include-dir", "a", "--include-dir", "b"];
        match parse(&argv).command {
            Command::Build(a) => {
                assert_eq!(a.include_dir, ["a", "b"]);
            }
            _ => panic!("not build"),
        }
        let argv = [
            "sieve", "--dir", "nope", "--dir", "sieve", "ask", "q", "-n", "1", "-n", "2",
        ];
        let cli = parse(&argv);
        assert_eq!(
            cli.context_dir.as_deref(),
            Some(std::path::Path::new("sieve"))
        );
        match cli.command {
            Command::Ask(a) => assert_eq!(a.limit, 2),
            _ => panic!("not ask"),
        }
    }
}
