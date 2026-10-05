//! Commander's error forms for the CLI (P1-01, P1-51). The parser
//! prints one line on stderr and exits 1. Clap prints its own text and
//! exits 2. This module holds the one map from clap's error kinds to
//! commander's text, and the ports of commander's top-level scan and
//! `suggestSimilar`. Sources: the `commander` package files `command.js` and
//! `suggestSimilar.js`, version 15.0.0.

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::Command;

/// The top-level value flags Commander registers on `program`, with the
/// `flags` text of `optionMissingArgument` (`.option(...)`).
pub const VALUE_FLAG_TEXT: &[(&str, &str)] = &[("--dir", "--dir <path>")];

/// The long flags Commander offers as top-level suggestions, help included.
const ROOT_LONG_FLAGS: &[&str] = &["--version", "--dir", "--help"];

/// The `flags` text of each value option of each subcommand:
/// `(subcommand, long flag, flags text)`.
const SUB_FLAG_TEXT: &[(&str, &str, &str)] = &[
    ("build", "--extensions", "-e, --extensions <exts...>"),
    ("build", "--include-dir", "--include-dir <name>"),
    ("build", "--only-dir", "--only-dir <path>"),
    ("ask", "--limit", "-n, --limit <n>"),
    ("ask", "--in", "--in <path>"),
    ("check", "--extensions", "-e, --extensions <exts...>"),
    ("viz", "--port", "-p, --port <port>"),
    ("viz", "--export", "--export <dir>"),
    ("viz", "--title", "--title <text>"),
    ("viz", "--tabs", "--tabs <list>"),
    ("callers", "--direction", "--direction <in|out>"),
    ("callers", "--depth", "-d, --depth <n>"),
    ("callers", "--in", "--in <path>"),
    ("blast", "--base", "--base <ref>"),
    ("blast", "--depth", "-d, --depth <n>"),
    ("blast", "--format", "--format <fmt>"),
    ("blast", "--export-viz", "--export-viz <dir>"),
    ("blast", "--title", "--title <text>"),
    ("blast", "--pr-author", "--pr-author <who...>"),
    ("grep", "--in", "--in <path>"),
    ("map", "--max-dirs", "--max-dirs <n>"),
    ("init", "--agents", "--agents <ids...>"),
    ("init", "--trail", "--trail <handoff>"),
];

/// What Commander's top-level scan does before it routes to a subcommand.
#[derive(Debug, PartialEq, Eq)]
pub enum TopScan {
    /// `-v`, `--version` or a `-v...` group: print the version, exit 0.
    Version,
    /// A top-level value flag has no value: print this line, exit 1.
    MissingValue(&'static str),
    /// Every token was a known option: print the help to stderr, exit 1.
    NoOperands,
}

/// Reports whether `arg` is `--flag=value` for a top-level value flag
/// (the `--foo=bar` branch of `parseOptions`).
pub fn is_inline_value_flag(arg: &str) -> bool {
    arg.split_once('=').is_some_and(|(name, _)| {
        name.len() > 2 && VALUE_FLAG_TEXT.iter().any(|(flag, _)| *flag == name)
    })
}

/// Reports whether `arg` is a top-level value flag, with no `=`.
fn is_root_value_flag(arg: &str) -> bool {
    VALUE_FLAG_TEXT.iter().any(|(flag, _)| *flag == arg)
}

/// Ports the top-level `parseOptions` loop for the faults that stop it at once.
/// Commander reads every token left to right, subcommand arguments included, so
/// a version flag anywhere wins. Returns `None` when the scan finishes with an
/// operand left.
pub fn scan_top_level(args: &[String]) -> Option<TopScan> {
    let mut operand_seen = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if arg == "--" {
            operand_seen |= i < args.len();
            break;
        }
        if arg == "-v" || arg == "--version" {
            return Some(TopScan::Version);
        }
        if let Some((_, flags)) = VALUE_FLAG_TEXT.iter().find(|(flag, _)| *flag == arg) {
            if i >= args.len() {
                return Some(TopScan::MissingValue(flags));
            }
            i += 1;
            continue;
        }
        // A `-v` group such as `-vh`: the boolean `-v` fires at once.
        if arg.len() > 2 && arg.starts_with("-v") {
            return Some(TopScan::Version);
        }
        if is_inline_value_flag(arg) {
            continue;
        }
        operand_seen = true;
    }
    (!operand_seen).then_some(TopScan::NoOperands)
}

/// Ports `editDistance`, the optimal string alignment distance.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    const MAX_DISTANCE: usize = 3;
    if a.len().abs_diff(b.len()) > MAX_DISTANCE {
        return a.len().max(b.len());
    }
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for j in 1..=b.len() {
        for i in 1..=a.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// Ports `suggestSimilar`: the text that follows an `unknown option` line, or
/// an empty string.
pub fn suggest_similar(word: &str, candidates: &[String]) -> String {
    const MAX_DISTANCE: usize = 3;
    let searching_options = word.starts_with("--");
    let strip = |s: &str| {
        if searching_options {
            s.chars().skip(2).collect::<Vec<char>>()
        } else {
            s.chars().collect()
        }
    };
    let word_chars = strip(word);
    let mut seen: Vec<&String> = Vec::new();
    let mut similar: Vec<String> = Vec::new();
    let mut best = MAX_DISTANCE;
    for candidate in candidates {
        if seen.contains(&candidate) {
            continue;
        }
        seen.push(candidate);
        let chars = strip(candidate);
        if chars.len() <= 1 {
            continue;
        }
        let distance = edit_distance(&word_chars, &chars);
        let length = word_chars.len().max(chars.len());
        let similarity = (length - distance.min(length)) as f64 / length as f64;
        if similarity > 0.4 {
            let text: String = chars.iter().collect();
            if distance < best {
                best = distance;
                similar = vec![text];
            } else if distance == best {
                similar.push(text);
            }
        }
    }
    similar.sort_by(|a, b| sieve_core::collate::collate(a, b));
    if searching_options {
        similar = similar.into_iter().map(|s| format!("--{s}")).collect();
    }
    match similar.as_slice() {
        [] => String::new(),
        [one] => format!("\n(Did you mean {one}?)"),
        many => format!("\n(Did you mean one of {}?)", many.join(", ")),
    }
}

/// The `unknown option` line for top-level `flag`, suggestion included
/// (`unknownOption`).
pub fn unknown_option_root(flag: &str) -> String {
    let candidates: Vec<String> = ROOT_LONG_FLAGS.iter().map(|s| s.to_string()).collect();
    format!(
        "error: unknown option '{flag}'{}",
        suggest_candidates(flag, &candidates)
    )
}

/// Applies `suggestSimilar` only to a `--long` flag, as `unknownOption` does.
fn suggest_candidates(flag: &str, candidates: &[String]) -> String {
    if flag.starts_with("--") {
        suggest_similar(flag, candidates)
    } else {
        String::new()
    }
}

/// The result of Commander's leaf `parseOptions` over one subcommand's
/// arguments.
#[derive(Default)]
struct SubScan {
    /// The operands, in order (`this.args` for a leaf with no unknown option).
    operands: Vec<String>,
    /// The first unknown option, as typed.
    unknown: Option<String>,
    /// The long flag of the first value option with no value.
    missing_value: Option<String>,
    /// Every token that is not an operand, in order. A value that starts
    /// with `-` becomes `--long=value`.
    options: Vec<String>,
    /// A `-h` or `--help` token sat among the unknown options. Commander
    /// lists no help option, so it finds the token there
    /// (the commander help check).
    help: bool,
    /// The argv needs a rewrite for clap: an option value starts with `-`.
    rewrite: bool,
}

/// Reports whether `arg` can be an option (`maybeOption`).
fn maybe_option(arg: &str) -> bool {
    arg.len() > 1 && arg.starts_with('-')
}

/// Finds the option that `flag` names, `--long` or `-s`.
fn find_option<'a>(sub: &'a Command, flag: &str) -> Option<&'a clap::Arg> {
    sub.get_arguments().find(|a| {
        !a.is_positional()
            && a.get_id() != "help"
            && (a.get_long().is_some_and(|l| flag == format!("--{l}"))
                || a.get_short().is_some_and(|s| flag == format!("-{s}")))
    })
}

/// Reports whether `arg` is a negative number (`negativeNumberArg`):
/// `-5`, `-.5`, `-1.5`, `-1e5`. In a leaf command it
/// is an operand or a variadic value, never an unknown option.
fn negative_number(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('-') else {
        return false;
    };
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    let int = digits(rest);
    let mut rest = &rest[int..];
    let mut frac = 0;
    if let Some(after) = rest.strip_prefix('.') {
        frac = digits(after);
        if frac == 0 {
            return false;
        }
        rest = &after[frac..];
    }
    if int == 0 && frac == 0 {
        return false;
    }
    match rest.strip_prefix('e') {
        Some(exp) => {
            let exp = exp.strip_prefix(['+', '-']).unwrap_or(exp);
            !exp.is_empty() && exp.bytes().all(|b| b.is_ascii_digit())
        }
        None => rest.is_empty(),
    }
}

/// Ports the leaf `parseOptions` loop over `rest`,
/// the tokens after the subcommand name. The option table is clap's.
fn scan_sub(sub: &Command, rest: &[String]) -> SubScan {
    let mut scan = SubScan::default();
    // The long flag of the variadic option that still takes values.
    let mut variadic: Option<String> = None;
    // The rest of a short group such as `-ix` after a boolean flag
    // (`activeGroup`).
    let mut group: Option<String> = None;
    let mut i = 0;
    while i < rest.len() || group.is_some() {
        let owned: String;
        let from_group = group.is_some();
        let arg: &str = match group.take() {
            Some(g) => {
                owned = g;
                &owned
            }
            None => {
                i += 1;
                &rest[i - 1]
            }
        };
        if arg == "--" {
            scan.operands.extend(rest[i..].iter().cloned());
            break;
        }
        // The root command reads its own options first, wherever they sit
        // (the root option parse), and a variadic run goes on past
        // them. Clap knows only `--dir`, so the other root flags and their
        // values leave the argv for clap.
        let root_flag = is_root_value_flag(arg);
        if root_flag || is_inline_value_flag(arg) {
            if arg == "--dir" || arg.starts_with("--dir=") {
                scan.options.push(arg.to_string());
                if let (true, Some(value)) = (root_flag, rest.get(i)) {
                    scan.options.push(value.clone());
                }
            } else {
                scan.rewrite = true;
            }
            i += usize::from(root_flag);
            continue;
        }
        if let Some(long) = &variadic {
            if !maybe_option(arg) {
                // The `=` form keeps the value in the variadic option,
                // after an earlier `--long=-x` value ended clap's run.
                scan.options.push(format!("--{long}={arg}"));
                continue;
            }
            if negative_number(arg) {
                scan.options.push(format!("--{long}={arg}"));
                continue;
            }
        }
        variadic = None;
        if !from_group {
            scan.options.push(arg.to_string());
        }
        if maybe_option(arg) {
            if let Some(option) = find_option(sub, arg) {
                if option.get_action().takes_values() {
                    // A root flag after a value option is not its value.
                    let root_next = rest
                        .get(i)
                        .is_some_and(|v| is_root_value_flag(v) || is_inline_value_flag(v));
                    if i >= rest.len() || root_next {
                        scan.missing_value = option.get_long().map(|l| format!("--{l}"));
                        return scan;
                    }
                    // A value that starts with `-` is still the value.
                    // Clap needs the `=` form.
                    match option.get_long() {
                        Some(long) if !from_group && rest[i].starts_with('-') => {
                            scan.options.pop();
                            scan.options.push(format!("--{long}={}", rest[i]));
                            scan.rewrite = true;
                        }
                        _ => scan.options.push(rest[i].clone()),
                    }
                    i += 1;
                    if option.get_num_args().is_some_and(|r| r.max_values() > 1) {
                        variadic = option.get_long().map(str::to_string);
                    }
                }
                continue;
            }
            // A short group such as `-n5` or `-ix`: the first letter picks
            // the option. A value option takes the rest as its value. A
            // boolean option leaves the rest as the next group.
            if arg.len() > 2 && !arg.starts_with("--") {
                let first: String = arg.chars().take(2).collect();
                if let Some(option) = find_option(sub, &first) {
                    let attached = &arg[first.len()..];
                    match option.get_long() {
                        _ if !option.get_action().takes_values() => {
                            group = Some(format!("-{attached}"));
                        }
                        Some(long) if !from_group => {
                            scan.options.pop();
                            scan.options.push(format!("--{long}={attached}"));
                            scan.rewrite = true;
                        }
                        _ => {}
                    }
                    continue;
                }
            }
            if let Some((name, _)) = arg.split_once('=') {
                if name.starts_with("--")
                    && find_option(sub, name).is_some_and(|o| o.get_action().takes_values())
                {
                    continue;
                }
            }
            if !negative_number(arg) {
                scan.unknown.get_or_insert_with(|| arg.to_string());
                scan.help |= arg == "-h" || arg == "--help";
            }
        }
        if !from_group {
            scan.options.pop();
        }
        scan.operands.push(arg.to_string());
    }
    scan
}

/// Finds the index of the subcommand name in `raw`, past top-level value
/// flags and their values.
fn sub_index(root: &Command, raw: &[String]) -> Option<usize> {
    let mut i = 0;
    loop {
        let arg = raw.get(i)?;
        if VALUE_FLAG_TEXT.iter().any(|(flag, _)| flag == arg) {
            i += 2;
        } else if root.find_subcommand(arg).is_some() {
            return Some(i);
        } else {
            i += 1;
        }
    }
}

/// Rewrites `raw` for clap when a subcommand line holds a negative number
/// that commander takes as an operand or a variadic value. Clap's own
/// number test differs from commander's regex, so commander's scan decides.
/// The operands move behind a `--`, in order, and a variadic value becomes
/// `--long=value`. Returns `None` when no rewrite is needed, or when the
/// line has an unknown option or a missing value, which clap then reports.
pub fn rewrite_negative_numbers(root: &Command, raw: &[String]) -> Option<Vec<String>> {
    let index = sub_index(root, raw)?;
    let sub = root.find_subcommand(&raw[index])?;
    let rest = &raw[index + 1..];
    let scan = scan_sub(sub, rest);
    if scan.unknown.is_some() || scan.missing_value.is_some() {
        return None;
    }
    if !scan.rewrite && !rest.iter().any(|a| negative_number(a)) {
        return None;
    }
    let mut out: Vec<String> = raw[..=index].to_vec();
    out.extend(scan.options);
    out.push("--".to_string());
    out.extend(scan.operands);
    Some(out)
}

/// Reads the first string that clap stored under `kind`.
fn context_string(err: &clap::Error, kind: ContextKind) -> Option<String> {
    match err.get(kind)? {
        ContextValue::String(s) => Some(s.clone()),
        ContextValue::Strings(v) => v.first().cloned(),
        _ => None,
    }
}

/// The line for a missing option value, then an unknown option, in
/// commander's order (`optionMissingArgument`, `unknownOption`).
fn option_fault(sub: &Command, scan: &SubScan) -> Option<String> {
    if let Some(long) = &scan.missing_value {
        let (_, _, flags) = SUB_FLAG_TEXT
            .iter()
            .find(|(name, l, _)| *name == sub.get_name() && l == long)?;
        return Some(format!("error: option '{flags}' argument missing"));
    }
    let flag = scan.unknown.as_ref()?;
    // Known limit: the candidates are the flags Sieve declares, so a
    // flag that only Sieve has can change a hint.
    let mut candidates: Vec<String> = sub
        .get_arguments()
        .filter(|a| !a.is_hide_set())
        .filter_map(|a| a.get_long().map(|l| format!("--{l}")))
        .collect();
    candidates.extend(ROOT_LONG_FLAGS.iter().map(|s| s.to_string()));
    Some(format!(
        "error: unknown option '{flag}'{}",
        suggest_candidates(flag, &candidates)
    ))
}

/// What commander does with a subcommand line before any operand check.
pub enum Precheck {
    /// Print this line on stderr, exit 1.
    Line(String),
    /// Print the subcommand help, exit 0. Clap prints it for this argv.
    Help(Vec<String>),
}

/// Ports commander's order for the option faults: a missing option value
/// throws first, a `-h` or `--help` token wins over an unknown option,
/// and an unknown option comes before any operand error. Returns `None`
/// when the line has none of these, so clap parses it.
pub fn precheck(root: &Command, raw: &[String]) -> Option<Precheck> {
    let index = sub_index(root, raw)?;
    let sub = root.find_subcommand(&raw[index])?;
    let scan = scan_sub(sub, &raw[index + 1..]);
    if scan.missing_value.is_none() && scan.help {
        let mut argv = raw[..=index].to_vec();
        argv.push("--help".to_string());
        return Some(Precheck::Help(argv));
    }
    option_fault(sub, &scan).map(Precheck::Line)
}

/// The map from clap's error kinds to commander's one-line messages.
///
/// - `UnknownArgument` with an unknown option: `unknownOption`,
///   with the suggestion.
/// - `UnknownArgument` with an extra operand: `_excessArguments`.
/// - `MissingRequiredArgument`: `missingArgument`.
/// - `InvalidValue`, `WrongNumberOfValues`, `TooFewValues`: with no
///   value: `optionMissingArgument`.
///
/// Commander reads the whole argument list before it reports, so an
/// option with no value wins over an unknown option, and an unknown
/// option wins over a missing or extra operand. `scan_sub` decides those
/// two; the clap kind decides the rest. Returns `None` for a kind that
/// has no commander form, such as a help request.
pub fn line_for(err: &clap::Error, root: &Command, raw: &[String]) -> Option<String> {
    let sub_index = sub_index(root, raw)?;
    let sub = root.find_subcommand(&raw[sub_index])?;
    let rest = &raw[sub_index + 1..];
    let scan = scan_sub(sub, rest);
    match err.kind() {
        ErrorKind::InvalidValue
        | ErrorKind::WrongNumberOfValues
        | ErrorKind::TooFewValues
        | ErrorKind::UnknownArgument
        | ErrorKind::MissingRequiredArgument => {}
        _ => return None,
    }
    if let Some(line) = option_fault(sub, &scan) {
        return Some(line);
    }
    match err.kind() {
        ErrorKind::MissingRequiredArgument => {
            // Known limit: the name is clap's value name in lower case. It
            // matches the help text only for query, pattern, symbol and file.
            let name = context_string(err, ContextKind::InvalidArg)?;
            let name = name.trim_matches(|c| c == '<' || c == '>').to_lowercase();
            Some(format!("error: missing required argument '{name}'"))
        }
        ErrorKind::UnknownArgument => {
            let expected = sub.get_positionals().count();
            // Commander sees no excess here, so clap's text stays.
            if scan.operands.len() <= expected {
                return None;
            }
            let s = if expected == 1 { "" } else { "s" };
            Some(format!(
                "error: too many arguments for '{}'. Expected {expected} argument{s} but got {}: {}.",
                sub.get_name(),
                scan.operands.len(),
                scan.operands.join(", ")
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// P1-51: the suggestion text matches commander's for one near match,
    /// a tie, and a far word.
    #[test]
    fn test_p1_51_suggest_similar_matches_commander() {
        let flags = strings(&["--version", "--dir", "--help"]);
        assert_eq!(
            suggest_similar("--versio", &flags),
            "\n(Did you mean --version?)"
        );
        assert_eq!(suggest_similar("--zzzzzzzz", &flags), "");
        let tie = strings(&["--aab", "--aac"]);
        assert_eq!(
            suggest_similar("--aad", &tie),
            "\n(Did you mean one of --aab, --aac?)"
        );
    }

    /// P1-51: tied suggestions sort by ICU collation, as `localeCompare`
    /// does. A byte sort puts `-` before `_`. ICU puts `_` first.
    #[test]
    fn test_p1_51_suggestions_sort_by_collation() {
        let tie = strings(&["--a-b", "--a_b"]);
        assert_eq!(
            suggest_similar("--axb", &tie),
            "\n(Did you mean one of --a_b, --a-b?)"
        );
    }

    /// P1-51: `negative_number` follows commander's regex
    /// `^-(\d+|\d*\.\d+)(e[+-]?\d+)?$`.
    #[test]
    fn test_p1_51_negative_number_matches_commander_regex() {
        for yes in ["-5", "-.5", "-1.5", "-1e5", "-1.5e-3"] {
            assert!(negative_number(yes), "{yes}");
        }
        for no in ["-", "-x", "-5.", "-1e", "-e5", "5", "-5x", "--5"] {
            assert!(!negative_number(no), "{no}");
        }
    }

    /// P1-51: a version flag anywhere beats a later fault, and a `-v`
    /// group fires the version at once.
    #[test]
    fn test_p1_51_scan_top_level_orders_faults() {
        assert_eq!(scan_top_level(&strings(&["-vh"])), Some(TopScan::Version));
        assert_eq!(
            scan_top_level(&strings(&["-h", "-v"])),
            Some(TopScan::Version)
        );
        assert_eq!(
            scan_top_level(&strings(&["--dir"])),
            Some(TopScan::MissingValue("--dir <path>"))
        );
        assert_eq!(
            scan_top_level(&strings(&["--dir=x"])),
            Some(TopScan::NoOperands)
        );
        assert_eq!(scan_top_level(&strings(&["--dir", "x", "ask"])), None);
    }
}
