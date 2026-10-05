//! The terminal voice: Tokyo Night color, the small mascot, and the one
//! error line `sieve: <what went wrong> — <what to run>`.
//!
//! Color shows only on a terminal. `NO_COLOR` or `TERM=dumb` turns it off.
//! A pipe gets the same words with no escape code.

use std::io::IsTerminal;
use std::path::Path;

use serde_json::Value;

/// One 24-bit color.
pub type Rgb = (u8, u8, u8);

/// The Tokyo Night colors of the CLI voice.
pub const FG: Rgb = (0xc0, 0xca, 0xf5);
/// Dim text.
pub const DIM: Rgb = (0x9a, 0xa5, 0xce);
/// Blue: counts and numbers.
pub const BLUE: Rgb = (0x7a, 0xa2, 0xf7);
/// Cyan: paths.
pub const CYAN: Rgb = (0x7d, 0xcf, 0xff);
/// Green: success.
pub const GREEN: Rgb = (0x9e, 0xce, 0x6a);
/// Orange: warnings and results of a change.
pub const ORANGE: Rgb = (0xff, 0x9e, 0x64);
/// Red: errors.
pub const RED: Rgb = (0xf7, 0x76, 0x8e);
/// Purple: names.
pub const PURPLE: Rgb = (0xbb, 0x9a, 0xf7);
/// Yellow: notes.
pub const YELLOW: Rgb = (0xe0, 0xaf, 0x68);

const RESET: &str = "\x1b[0m";
const MASCOTS: &str = include_str!("../assets/mascots.json");

/// How many colors a stream takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// No escape code.
    Plain,
    /// The 256-color codes.
    Ansi256,
    /// The 24-bit codes.
    True,
}

/// Picks the color depth for one stream.
pub fn depth_from(
    is_terminal: bool,
    no_color: Option<&str>,
    term: Option<&str>,
    colorterm: Option<&str>,
) -> Depth {
    if !is_terminal || no_color.is_some_and(|v| !v.is_empty()) || term == Some("dumb") {
        Depth::Plain
    } else if matches!(colorterm, Some("truecolor" | "24bit")) {
        Depth::True
    } else {
        Depth::Ansi256
    }
}

/// The nearest 256-color index in the 6x6x6 color cube.
fn nearest_256(c: Rgb) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let step = |v: u8| {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] - i32::from(v)).abs())
            .unwrap_or(0) as u8
    };
    16 + 36 * step(c.0) + 6 * step(c.1) + step(c.2)
}

/// The escape code for a foreground (`layer` 38) or background (`layer` 48).
fn code(layer: u8, c: Rgb, depth: Depth) -> String {
    match depth {
        Depth::Plain => String::new(),
        Depth::Ansi256 => format!("\x1b[{layer};5;{}m", nearest_256(c)),
        Depth::True => format!("\x1b[{layer};2;{};{};{}m", c.0, c.1, c.2),
    }
}

/// A paint set for one output stream.
#[derive(Clone, Copy, Debug)]
pub struct Ui {
    /// The color depth of the stream.
    pub depth: Depth,
}

impl Ui {
    /// The paint set for stdout.
    pub fn stdout() -> Ui {
        Ui::for_stream(std::io::stdout().is_terminal())
    }

    /// The paint set for stderr.
    pub fn stderr() -> Ui {
        Ui::for_stream(std::io::stderr().is_terminal())
    }

    fn for_stream(is_terminal: bool) -> Ui {
        let get = |k: &str| std::env::var(k).ok();
        Ui {
            depth: depth_from(
                is_terminal,
                get("NO_COLOR").as_deref(),
                get("TERM").as_deref(),
                get("COLORTERM").as_deref(),
            ),
        }
    }

    /// A paint set with no color, for a test.
    #[cfg(test)]
    pub fn plain() -> Ui {
        Ui {
            depth: Depth::Plain,
        }
    }

    /// True when this stream takes color.
    pub fn on(&self) -> bool {
        self.depth != Depth::Plain
    }

    /// Paints `text` with the foreground color `c`.
    pub fn paint(&self, c: Rgb, text: &str) -> String {
        if self.on() {
            format!("{}{text}{RESET}", code(38, c, self.depth))
        } else {
            text.to_string()
        }
    }

    /// Dim text.
    pub fn dim(&self, text: &str) -> String {
        self.paint(DIM, text)
    }
    /// Blue text, for counts.
    pub fn blue(&self, text: &str) -> String {
        self.paint(BLUE, text)
    }
    /// Cyan text, for paths.
    pub fn cyan(&self, text: &str) -> String {
        self.paint(CYAN, text)
    }
    /// Green text, for success.
    pub fn green(&self, text: &str) -> String {
        self.paint(GREEN, text)
    }
    /// Orange text, for a change or a warning.
    pub fn orange(&self, text: &str) -> String {
        self.paint(ORANGE, text)
    }
    /// Red text, for an error.
    pub fn red(&self, text: &str) -> String {
        self.paint(RED, text)
    }
    /// Purple text, for names.
    pub fn purple(&self, text: &str) -> String {
        self.paint(PURPLE, text)
    }
    /// Yellow text, for a note.
    pub fn yellow(&self, text: &str) -> String {
        self.paint(YELLOW, text)
    }
    /// Plain foreground text.
    pub fn fg(&self, text: &str) -> String {
        self.paint(FG, text)
    }
}

/// Reads one hex color such as `#7aa2f7`.
fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Reads `mascot` from `<root>/.sieve/config.json`. The default is `sieve`.
fn configured_mascot(root: &Path) -> String {
    let dir = sieve_core::product().home_dir_name();
    std::fs::read_to_string(root.join(dir).join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("mascot")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "sieve".to_string())
}

/// The 6 pixel rows of the small frame 0 of one mascot.
fn small_pixels(all: &Value, name: &str) -> Option<Vec<Vec<Option<Rgb>>>> {
    let rows = all
        .get("mascots")?
        .get(name)?
        .get("small")?
        .get(0)?
        .as_array()?;
    let palette = all.get("palette")?;
    let color_of = |c: char| parse_hex(palette.get(c.to_string())?.as_str()?);
    let px: Vec<Vec<Option<Rgb>>> = rows
        .iter()
        .filter_map(Value::as_str)
        .map(|r| r.chars().map(color_of).collect())
        .collect();
    Some(px).filter(|p| p.len() == 6)
}

/// Draws the small mascot as 3 terminal rows of half blocks. A cell with no
/// pixel is U+2800, which is one column wide and not white space.
///
/// Returns `None` when the stream takes no color, or the config names
/// `none`. An unknown name gives the default mascot.
pub fn mascot_rows(root: &Path, depth: Depth) -> Option<[String; 3]> {
    let name = configured_mascot(root);
    if depth == Depth::Plain || name == "none" {
        return None;
    }
    let all: Value = serde_json::from_str(MASCOTS).ok()?;
    let px = small_pixels(&all, &name).or_else(|| small_pixels(&all, "sieve"))?;
    let row = |top: &[Option<Rgb>], bottom: &[Option<Rgb>]| -> String {
        top.iter()
            .zip(bottom)
            .map(|pair| match pair {
                (None, None) => "\u{2800}".to_string(),
                (Some(t), None) => format!("{}\u{2580}{RESET}", code(38, *t, depth)),
                (None, Some(b)) => format!("{}\u{2584}{RESET}", code(38, *b, depth)),
                (Some(t), Some(b)) => format!(
                    "{}{}\u{2580}{RESET}",
                    code(38, *t, depth),
                    code(48, *b, depth)
                ),
            })
            .collect()
    };
    Some([
        row(&px[0], &px[1]),
        row(&px[2], &px[3]),
        row(&px[4], &px[5]),
    ])
}

/// The width of the small mascot in columns, plus a gap of two.
const MASCOT_INDENT: usize = 14;

/// Puts the mascot rows beside the first lines of `lines`. A line after the
/// third is indented to line up with the others.
pub fn beside_mascot(mascot: &[String; 3], lines: &[String]) -> String {
    let mut out = String::new();
    let rows = lines.len().max(3);
    for i in 0..rows {
        let text = lines.get(i).map_or("", String::as_str);
        match mascot.get(i) {
            Some(m) => out.push_str(&format!("{m}  {text}")),
            None => out.push_str(&format!("{}{text}", " ".repeat(MASCOT_INDENT))),
        }
        out.push('\n');
    }
    out
}

/// Builds the one error line: `sieve: <what went wrong> — <what to run>`.
///
/// `message` already holds the ` — ` that parts the two halves. If it does
/// not, the hint is `run sieve <command> --help`.
pub fn error_line(ui: &Ui, message: &str, command: Option<&str>) -> String {
    let name = sieve_core::product().name;
    let (what, hint) = match message.split_once(" \u{2014} ") {
        Some((what, hint)) => (what.to_string(), hint.to_string()),
        None => {
            let errno = message.split_once(": ").is_some_and(|(code, _)| {
                code.len() > 1
                    && code.starts_with('E')
                    && code.bytes().all(|b| b.is_ascii_uppercase())
            });
            let help = match command {
                _ if errno => "check the path and its permissions".to_string(),
                Some(c) => format!("run {name} {c} --help"),
                None => format!("run {name} --help"),
            };
            (message.to_string(), help)
        }
    };
    format!(
        "{} {} {} {hint}",
        ui.red(&format!("{name}:")),
        ui.fg(&what),
        ui.dim("\u{2014}")
    )
}

/// The subcommands that take part in the error hint.
const COMMANDS: &[&str] = &[
    "build",
    "ask",
    "grep",
    "skeleton",
    "callers",
    "blast",
    "map",
    "check",
    "init",
    "uninstall",
    "version",
    "stats",
    "telemetry",
    "viz",
    "why",
    "mcp",
];

/// The subcommand of this run: the first operand of the command line, with
/// `--dir <path>` skipped. `None` when the line has no operand.
pub fn current_command() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--dir" {
            args.next();
        } else if COMMANDS.contains(&arg.as_str()) {
            return Some(arg);
        }
    }
    None
}

/// Prints the one error line to stderr, for this run's subcommand.
pub fn print_error(message: &str) {
    let command = current_command();
    eprintln!("{}", error_line(&Ui::stderr(), message, command.as_deref()));
}

/// Prints one Commander-style line (`error: unknown option '--nope'`) to
/// stderr in the error shape.
pub fn print_commander_error(line: &str) {
    let command = current_command();
    print_error(&commander_message(line, command.as_deref()));
}

/// Turns one Commander-style line (`error: unknown option '--nope'`) into
/// the message that [`error_line`] takes.
pub fn commander_message(line: &str, command: Option<&str>) -> String {
    let name = sieve_core::product().name;
    let help = match command {
        Some(c) => format!("see {name} {c} --help"),
        None => format!("see {name} --help"),
    };
    let mut parts = line.lines();
    let first = parts.next().unwrap_or("");
    let suggest = parts
        .next()
        .map(|s| s.trim_matches(|c| c == '(' || c == ')'))
        .map(|s| s.replace("Did you mean", "did you mean"))
        .map(|s| s.trim_end_matches('?').to_string());
    let body = first.strip_prefix("error: ").unwrap_or(first);
    let quoted = |text: &str| -> Option<String> {
        let start = text.find('\'')? + 1;
        let end = text[start..].find('\'')? + start;
        Some(text[start..end].to_string())
    };
    let what = if body.starts_with("unknown option") {
        let flag = quoted(body).unwrap_or_default();
        match command {
            Some(c) => format!("{c} has no {flag} option"),
            None => format!("{name} has no {flag} option"),
        }
    } else if body.starts_with("unknown command") {
        format!("no command named {}", quoted(body).unwrap_or_default())
    } else if body.starts_with("missing required argument") {
        let arg = quoted(body).unwrap_or_default();
        return match (command, arg.as_str()) {
            (Some("ask"), "query") => {
                format!("ask needs a question \u{2014} run {name} ask \"how does login work\"")
            }
            (Some(c), a) => format!("{c} needs a {a} \u{2014} {help}"),
            (None, a) => format!("{name} needs a {a} \u{2014} {help}"),
        };
    } else if body.starts_with("option ") && body.ends_with("argument missing") {
        let flag = quoted(body).unwrap_or_default();
        let flag = flag.split_whitespace().next().unwrap_or("").to_string();
        format!("{flag} needs a value")
    } else {
        body.to_string()
    };
    match suggest {
        Some(s) => format!("{what} \u{2014} {s}, or {help}"),
        None => format!("{what} \u{2014} {help}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_needs_a_terminal_and_no_opt_out() {
        assert_eq!(depth_from(false, None, Some("xterm"), None), Depth::Plain);
        assert_eq!(
            depth_from(true, Some("1"), Some("xterm"), None),
            Depth::Plain
        );
        assert_eq!(depth_from(true, None, Some("dumb"), None), Depth::Plain);
        assert_eq!(depth_from(true, None, Some("xterm"), None), Depth::Ansi256);
        assert_eq!(
            depth_from(true, None, Some("xterm"), Some("truecolor")),
            Depth::True
        );
        assert_eq!(
            depth_from(true, None, Some("xterm"), Some("24bit")),
            Depth::True
        );
    }

    #[test]
    fn paint_uses_24_bit_or_the_nearest_256_color() {
        let tc = Ui { depth: Depth::True };
        assert_eq!(tc.red("x"), "\x1b[38;2;247;118;142mx\x1b[0m");
        let c256 = Ui {
            depth: Depth::Ansi256,
        };
        assert_eq!(c256.blue("x"), "\x1b[38;5;111mx\x1b[0m");
        assert_eq!(Ui::plain().red("x"), "x");
    }

    #[test]
    fn error_line_has_one_shape() {
        let ui = Ui::plain();
        assert_eq!(
            error_line(
                &ui,
                "no index here yet \u{2014} run sieve build .",
                Some("ask")
            ),
            "sieve: no index here yet \u{2014} run sieve build ."
        );
        assert_eq!(
            error_line(&ui, "boom", Some("map")),
            "sieve: boom \u{2014} run sieve map --help"
        );
        assert_eq!(
            error_line(&ui, "EACCES: permission denied, scandir '/x'", Some("build")),
            "sieve: EACCES: permission denied, scandir '/x' \u{2014} check the path and its permissions"
        );
        assert_eq!(
            error_line(&ui, "boom", None),
            "sieve: boom \u{2014} run sieve --help"
        );
    }

    #[test]
    fn commander_lines_use_the_error_shape() {
        assert_eq!(
            commander_message("error: unknown option '--nope'", Some("build")),
            "build has no --nope option \u{2014} see sieve build --help"
        );
        assert_eq!(
            commander_message("error: missing required argument 'query'", Some("ask")),
            "ask needs a question \u{2014} run sieve ask \"how does login work\""
        );
        assert_eq!(
            commander_message("error: unknown command 'foo'", None),
            "no command named foo \u{2014} see sieve --help"
        );
        assert_eq!(
            commander_message("error: option '--dir <path>' argument missing", None),
            "--dir needs a value \u{2014} see sieve --help"
        );
        assert_eq!(
            commander_message(
                "error: unknown option '--jsno'\n(Did you mean --json?)",
                Some("grep")
            ),
            "grep has no --jsno option \u{2014} did you mean --json, or see sieve grep --help"
        );
    }

    #[test]
    fn mascot_shows_only_with_color() {
        let dir = std::env::temp_dir();
        assert!(mascot_rows(&dir, Depth::Plain).is_none());
        let rows = mascot_rows(&dir, Depth::True).expect("a mascot");
        assert!(rows[0].contains('\u{2580}') || rows[0].contains('\u{2800}'));
        let text = beside_mascot(&rows, &["a".into(), "b".into(), "c".into(), "d".into()]);
        assert_eq!(text.lines().count(), 4);
        assert!(text
            .lines()
            .last()
            .is_some_and(|l| l == format!("{}d", " ".repeat(14))));
    }
}
