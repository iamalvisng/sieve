//! `sieve statusline`: the Claude Code statusline (the `hosts-hooks.md` note
//! section 4).

use std::io::Read as _;
use std::path::Path;

use clap::Args;
use serde_json::Value;
use sieve_core::product::product;
use sieve_core::wiring::{Graph, SummaryState};

use crate::hook::{self, SessionState, Stats};

/// Flags for `sieve statusline`. Takes no flags: every input arrives on
/// stdin as JSON.
#[derive(Args, Debug)]
pub struct StatuslineArgs {}

const INDIGO: &str = "\x1b[38;2;84;111;255m";
const MUTED: &str = "\x1b[38;5;244m";
const TEXT: &str = "\x1b[38;5;251m";
const RESET: &str = "\x1b[0m";

fn color(code: &str, s: &str) -> String {
    format!("{code}{s}{RESET}")
}

/// Runs `sieve statusline`: reads the canned status JSON, renders one or
/// two lines, no trailing newline.
pub fn run(_args: &StatuslineArgs, _dir_override: Option<&Path>) -> Result<(), String> {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let input: Value =
        serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(Default::default()));

    let project_dir = statusline_project_dir(&input);
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let session = hook::read_session(&project_dir, session_id);

    if let Some(agent) = input
        .get("agent")
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
    {
        print!("{}", render_subagent(agent, &session));
        return Ok(());
    }

    let stats = resolve_stats(&project_dir);
    let ctx_pct = input
        .get("context_window")
        .and_then(|c| c.get("used_percentage"))
        .and_then(Value::as_f64)
        .map(|p| p.round() as i64);
    print!(
        "{}",
        render_sieve_now(stats.as_ref(), &project_dir, ctx_pct)
    );
    Ok(())
}

fn statusline_project_dir(input: &Value) -> std::path::PathBuf {
    if let Ok(v) = std::env::var("CLAUDE_PROJECT_DIR") {
        if !v.is_empty() {
            return std::path::PathBuf::from(v);
        }
    }
    if let Some(cwd) = input.get("cwd").and_then(Value::as_str) {
        if !cwd.is_empty() {
            return std::path::PathBuf::from(cwd);
        }
    }
    std::env::current_dir().unwrap_or_default()
}

/// The stats snapshot the statusline renders (`resolveStats`): the cached
/// `stats.json` wins once it has at least one node, else `wiring.json`'s
/// own counts stand in for a build that never wrote the cache.
fn resolve_stats(project_dir: &Path) -> Option<Stats> {
    if let Some(cached) = hook::read_stats(project_dir) {
        if cached.node_count > 0 {
            return Some(cached);
        }
    }
    let context_dir = hook::resolve_context_dir(project_dir);
    // The build's own counts: no wiring parse.
    if let Some(c) = hook::read_build_counts(&context_dir) {
        return Some(Stats {
            node_count: c.node_count,
            edge_count: c.edge_count,
            languages: c.languages,
            total_count: c.total_count,
            ready_count: c.ready_count,
            capped: hook::wiring_over_cap(&context_dir),
            stale_counts: c.stale,
            ..Stats::default()
        });
    }
    // Over the size cap: show the stats there are, with no node count.
    if hook::wiring_over_cap(&context_dir) {
        let mut stats = hook::read_stats(project_dir).unwrap_or_default();
        stats.capped = true;
        return Some(stats);
    }
    let graph = hook::read_wiring(&context_dir)?;
    Some(Stats {
        node_count: graph.meta.node_count as u64,
        edge_count: graph.meta.edge_count as u64,
        languages: graph.meta.languages.clone(),
        total_count: graph.nodes.len() as u64,
        ready_count: count_ready(&graph),
        ..Stats::default()
    })
}

fn count_ready(graph: &Graph) -> u64 {
    graph
        .nodes
        .iter()
        .filter(|n| n.summary_state == SummaryState::Ready)
        .count() as u64
}

/// The one-line subagent form (`renderSubagent`).
fn render_subagent(agent_name: &str, session: &SessionState) -> String {
    let tail = match session.per_agent_query.get(agent_name) {
        Some(query) => format!(
            "{}{}{}",
            color(MUTED, " \u{b7} "),
            color(MUTED, &format!("{}: ", product().name)),
            color(TEXT, query)
        ),
        None => String::new(),
    };
    format!(
        "{}{}{tail}",
        color(MUTED, "\u{25e4} "),
        color(INDIGO, agent_name)
    )
}

fn format_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let bytes = digits.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        let from_end = bytes.len() - i;
        if i > 0 && from_end.is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

// ---------------------------------------------------------------------------
// The sieve status line: Tokyo Night colors and a small mascot.
// ---------------------------------------------------------------------------

/// The one source of the sprites. The Claude Code mod reads the same file.
const MASCOTS: &str = include_str!("../assets/mascots.json");

const SAND: Rgb = (232, 184, 109);
/// The border color. It gives 2.8:1 on the dark background, so it is for the separator only.
const DIM: Rgb = (86, 95, 137);
/// The dim text color, Tokyo Night fg_dark. It gives 7:1 on the dark background.
const FG_DARK: Rgb = (154, 165, 206);
const GREEN: Rgb = (158, 206, 106);
const BLUE: Rgb = (122, 162, 247);
const TEAL: Rgb = (115, 218, 202);
const ORANGE: Rgb = (255, 158, 100);

/// The seconds a cached savings total stays valid with no session change.
const SAVINGS_TTL_SECS: u64 = 30;

type Rgb = (u8, u8, u8);

/// How many colors the terminal takes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Depth {
    /// `NO_COLOR` is set: no escape code, no mascot.
    Plain,
    /// The 256-color codes.
    Ansi256,
    /// The 24-bit codes.
    True,
}

/// Picks the color depth from `NO_COLOR` and `COLORTERM`.
fn depth_from(no_color: Option<&str>, colorterm: Option<&str>) -> Depth {
    if no_color.is_some_and(|v| !v.is_empty()) {
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
            .min_by_key(|&i| (LEVELS[i] - v as i32).abs())
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

/// Everything the sieve status line shows, so a test can build one.
struct SieveView<'a> {
    /// `None` means the repo has no index yet.
    stats: Option<&'a Stats>,
    ctx_pct: Option<i64>,
    today: u64,
    week: u64,
    /// The two mascot rows, or `None` for no mascot.
    mascot: Option<[String; 2]>,
    depth: Depth,
}

/// Formats a token count as `950`, `1.5k`, `61.7k`, `214k` or `1.2M`.
fn format_short(n: u64) -> String {
    let (div, unit) = match n {
        0..=999 => return n.to_string(),
        1_000..=999_999 => (1_000.0, "k"),
        _ => (1_000_000.0, "M"),
    };
    let v = n as f64 / div;
    if v < 100.0 {
        format!("{:.1}{unit}", (v * 10.0).floor() / 10.0)
    } else {
        format!("{}{unit}", v.floor())
    }
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// The embedded sprite file, parsed once per process.
fn mascots() -> Option<&'static Value> {
    static PARSED: std::sync::OnceLock<Option<Value>> = std::sync::OnceLock::new();
    PARSED
        .get_or_init(|| serde_json::from_str(MASCOTS).ok())
        .as_ref()
}

/// Reads one hand-made tiny frame: 4 pixel rows, at most 8 columns. A plain
/// downscale of the large frame loses the eyes and the ears, so the `tiny`
/// frames in `mascots.json` are drawn by hand.
fn tiny_pixels(name: &str, frame: usize, mascots: &Value) -> Option<Vec<Vec<Option<Rgb>>>> {
    let rows = mascots
        .get("mascots")?
        .get(name)?
        .get("tiny")?
        .get(frame)?
        .as_array()?;
    let palette = mascots.get("palette")?;
    let color_of = |c: char| parse_hex(palette.get(c.to_string())?.as_str()?);
    let px: Vec<Vec<Option<Rgb>>> = rows
        .iter()
        .filter_map(Value::as_str)
        .map(|r| r.chars().map(color_of).collect())
        .collect();
    Some(px).filter(|p| p.len() == 4)
}

/// U+2800 BRAILLE PATTERN BLANK. It is one column wide and is not whitespace,
/// so Claude Code does not trim it from the start of a status line.
const BLANK: &str = "\u{2800}";

/// Draws the two terminal rows of the tiny mascot. A cell with a top
/// and a bottom pixel is an upper half block: the top color is the
/// foreground and the bottom color is the background. A cell with one
/// pixel is an upper or a lower half block.
fn mascot_rows(name: &str, frame: usize, depth: Depth) -> Option<[String; 2]> {
    let px = tiny_pixels(name, frame, mascots()?)?;
    let row = |top: &[Option<Rgb>], bottom: &[Option<Rgb>]| -> String {
        top.iter()
            .zip(bottom)
            .map(|pair| match pair {
                (None, None) => BLANK.to_string(),
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
    Some([row(&px[0], &px[1]), row(&px[2], &px[3])])
}

/// The mascot rows for a config name. `none` gives no mascot. An unknown
/// name gives the default `sieve` mascot.
fn mascot_for(name: &str, frame: usize, depth: Depth) -> Option<[String; 2]> {
    if name == "none" || depth == Depth::Plain {
        return None;
    }
    mascot_rows(name, frame, depth).or_else(|| mascot_rows("sieve", frame, depth))
}

/// The frame flips each second of the clock, so the mascot blinks.
fn frame_for(now: u64) -> usize {
    (now % 2) as usize
}

/// Reads `mascot` from `config.json` in the config dir (`<project>/.sieve`).
/// The default is `sieve`.
fn read_mascot(config_dir: &Path) -> String {
    std::fs::read_to_string(config_dir.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("mascot")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "sieve".to_string())
}

/// The newest change time, in nanoseconds, of the session dir and of its
/// files. A new or edited session file changes it.
fn session_stamp(session_dir: &Path) -> u64 {
    let nanos = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64)
    };
    let files = std::fs::read_dir(session_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| nanos(&e.path()));
    files.chain([nanos(session_dir)]).max().unwrap_or(0)
}

/// Returns the saved tokens (today, last 7 days) from a small cache file.
/// The cache stays valid while the session stamp is the same and under
/// `SAVINGS_TTL_SECS` old. Else `compute` runs and the cache is rewritten.
fn cached_savings(
    context_dir: &Path,
    now: u64,
    compute: impl FnOnce() -> (u64, u64),
) -> (u64, u64) {
    let stamp = session_stamp(&context_dir.join(".cache").join("session"));
    let path = context_dir.join(".cache").join("statusline-savings.json");
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
    let hit = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| field(v, "stamp") == Some(stamp))
        .filter(|v| field(v, "at").is_some_and(|at| now.saturating_sub(at) < SAVINGS_TTL_SECS))
        .and_then(|v| Some((field(&v, "today")?, field(&v, "week")?)));
    if let Some(totals) = hit {
        return totals;
    }
    let (today, week) = compute();
    let body = serde_json::json!({"stamp": stamp, "at": now, "today": today, "week": week});
    // A failed write only costs a recompute on the next run.
    let _ = std::fs::write(&path, body.to_string());
    (today, week)
}

/// Reads the saved tokens for today and for the last 7 days. The numbers
/// come from `savings_report`, the code behind `sieve stats --json`.
fn read_savings(project_dir: &Path, now: u64) -> (u64, u64) {
    let context_dir = crate::stats::stats_context_dir(project_dir);
    cached_savings(&context_dir, now, || {
        let report = crate::stats::savings_report(
            &context_dir,
            None,
            now,
            crate::telemetry::local_offset_secs(),
        );
        let tokens = |key: &str| {
            report
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.get("tokens"))
                .and_then(crate::jsonv::Json::as_f64)
                .unwrap_or(0.0) as u64
        };
        (tokens("today"), tokens("last7daysTotal"))
    })
}

/// Builds the view from the real environment and renders it.
fn render_sieve_now(stats: Option<&Stats>, project_dir: &Path, ctx_pct: Option<i64>) -> String {
    let depth = depth_from(
        std::env::var("NO_COLOR").ok().as_deref(),
        std::env::var("COLORTERM").ok().as_deref(),
    );
    let now = crate::telemetry::now_secs();
    render_sieve_at(stats, project_dir, ctx_pct, now, depth)
}

/// Renders the sieve status line for a given time and color depth.
fn render_sieve_at(
    stats: Option<&Stats>,
    project_dir: &Path,
    ctx_pct: Option<i64>,
    now: u64,
    depth: Depth,
) -> String {
    let name = read_mascot(&project_dir.join(product().home_dir_name()));
    let (today, week) = read_savings(project_dir, now);
    render_sieve(&SieveView {
        stats,
        ctx_pct,
        today,
        week,
        mascot: mascot_for(&name, frame_for(now), depth),
        depth,
    })
}

/// Renders the sieve status line: 3 lines, with the mascot beside lines 1 and 2.
fn render_sieve(v: &SieveView) -> String {
    let paint = |c: Rgb, s: &str| {
        if v.depth == Depth::Plain {
            s.to_string()
        } else {
            format!("{}{s}{RESET}", code(38, c, v.depth))
        }
    };
    // Each mascot row has the same width, so the text column lines up.
    let (cells1, cells2) = match &v.mascot {
        Some([a, b]) => (format!("{a}{BLANK}"), format!("{b}{BLANK}")),
        None => (String::new(), String::new()),
    };
    let head = format!(
        "{cells1}{} {} ",
        paint(SAND, "sieve"),
        paint(DIM, "\u{2502}")
    );
    let (line1, line2) = match v.stats {
        None => (
            format!("{head}{}", paint(FG_DARK, "no savings yet")),
            format!(
                "{cells2}{}",
                paint(FG_DARK, "no index yet \u{b7} run sieve build")
            ),
        ),
        Some(stats) => {
            let fresh = if stats.syncing {
                paint(ORANGE, "updating\u{2026}")
            } else if stats.dirty {
                paint(ORANGE, "out of date")
            } else {
                paint(TEAL, "\u{2713} up to date")
            };
            let counts = if stats.capped && stats.node_count == 0 {
                String::new()
            } else {
                format!(
                    "{}{} symbols \u{b7} {} links \u{b7} ",
                    if stats.stale_counts { "~" } else { "" },
                    format_thousands(stats.node_count),
                    format_thousands(stats.edge_count)
                )
            };
            let cap_note = if stats.capped {
                paint(
                    ORANGE,
                    " \u{b7} index over the size cap; hooks pass through",
                )
            } else {
                String::new()
            };
            (
                format!(
                    "{head}{}{}{}",
                    paint(GREEN, &format!("{} saved today", format_short(v.today))),
                    paint(FG_DARK, " \u{b7} "),
                    paint(BLUE, &format!("{} this week", format_short(v.week))),
                ),
                format!("{cells2}{}{fresh}{cap_note}", paint(FG_DARK, &counts)),
            )
        }
    };
    let mut lines = vec![line1, line2];
    // Line 3 has no mascot cells.
    lines.extend(v.ctx_pct.map(|pct| paint(FG_DARK, &format!("ctx {pct}%"))));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_esc = false;
        for c in s.chars() {
            match (in_esc, c) {
                (false, '\x1b') => in_esc = true,
                (true, 'm') => in_esc = false,
                (false, _) => out.push(c),
                _ => {}
            }
        }
        out
    }

    fn view_of(stats: &Stats, mascot: Option<[String; 2]>) -> SieveView<'_> {
        SieveView {
            stats: Some(stats),
            ctx_pct: Some(54),
            today: 61_700,
            week: 214_000,
            mascot,
            depth: Depth::True,
        }
    }

    fn view(stats: &Stats, mascot: Option<[String; 2]>, color: bool) -> String {
        render_sieve(&SieveView {
            depth: if color { Depth::True } else { Depth::Plain },
            ..view_of(stats, mascot)
        })
    }

    fn fixture_stats() -> Stats {
        Stats {
            node_count: 752,
            edge_count: 2017,
            ..Stats::default()
        }
    }

    #[test]
    fn test_statusline_sieve_has_mascot_cells_and_plain_words() {
        let stats = fixture_stats();
        let out = view(&stats, mascot_rows("sieve", 0, Depth::True), true);
        let text = strip_ansi(&out);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 3, "{text}");
        // The tiny sieve mascot is 8 cells wide, then one blank, then the text.
        let cut = |l: &str, n: usize| l.chars().skip(n).collect::<String>();
        for l in &lines[..2] {
            let cells: String = l.chars().take(8).collect();
            assert!(
                cells
                    .chars()
                    .all(|c| "\u{2800}\u{2580}\u{2584}".contains(c)),
                "{l}"
            );
            assert_eq!(l.chars().nth(8), Some('\u{2800}'), "{l}");
        }
        assert!(lines[0].contains('\u{2580}'), "{}", lines[0]);
        assert_eq!(
            cut(lines[0], 9),
            "sieve \u{2502} 61.7k saved today \u{b7} 214k this week"
        );
        assert_eq!(
            cut(lines[1], 9),
            "752 symbols \u{b7} 2,017 links \u{b7} \u{2713} up to date"
        );
        assert_eq!(lines[2], "ctx 54%", "line 3 has no mascot cells");
        let no_ctx = strip_ansi(&render_sieve(&SieveView {
            ctx_pct: None,
            ..view_of(&stats, mascot_rows("sieve", 0, Depth::True))
        }));
        assert_eq!(no_ctx.split('\n').count(), 2);
        assert!(out.contains("\x1b[48;2;"), "bottom pixels use a background");
    }

    #[test]
    fn test_statusline_mascot_survives_a_leading_whitespace_trim() {
        let stats = fixture_stats();
        let text = strip_ansi(&view(&stats, mascot_rows("sieve", 0, Depth::True), true));
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].starts_with("ctx"), "line 3 has no mascot cells");
        // `trim_start` removes Unicode White_Space, as JS trimStart does.
        let col = |l: &str, w: &str| l.find(w).map(|i| l[..i].chars().count());
        for (l, word) in lines.iter().zip(["sieve", "752"]) {
            assert!(!l.chars().next().is_some_and(char::is_whitespace), "{l}");
            assert_eq!(*l, l.trim_start(), "the trim removed cells");
            assert_eq!(col(l, word), Some(9), "{l}");
        }
    }

    #[test]
    fn test_statusline_sieve_stale_and_syncing_use_plain_words() {
        let mut stats = fixture_stats();
        stats.dirty = true;
        assert!(strip_ansi(&view(&stats, None, true)).contains("out of date"));
        stats.syncing = true;
        assert!(strip_ansi(&view(&stats, None, true)).contains("updating\u{2026}"));
    }

    #[test]
    fn test_statusline_no_color_gives_plain_text() {
        let stats = fixture_stats();
        let out = view(&stats, None, false);
        assert!(!out.contains('\x1b'), "{out}");
        assert_eq!(
            out,
            "sieve \u{2502} 61.7k saved today \u{b7} 214k this week\n\
             752 symbols \u{b7} 2,017 links \u{b7} \u{2713} up to date\nctx 54%"
        );
    }

    #[test]
    fn test_statusline_mascot_none_prints_no_mascot() {
        let dir = std::env::temp_dir().join(format!("sieve-sl-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("make dir");
        std::fs::write(dir.join("config.json"), r#"{"mascot":"none"}"#).expect("write config");
        let name = read_mascot(&dir);
        std::fs::remove_dir_all(&dir).expect("remove dir");
        assert_eq!(name, "none");
        assert_eq!(read_mascot(&dir), "sieve", "a missing config means sieve");
        let out = view(&fixture_stats(), None, true);
        assert!(!out.contains('\u{2580}') && !out.contains('\u{2584}'));
        assert!(out.starts_with(&code(38, SAND, Depth::True)), "{out}");
    }

    #[test]
    fn test_statusline_mascot_hoot_uses_hoot_colors() {
        let hoot = mascot_rows("hoot", 0, Depth::True).expect("hoot").join("");
        let soya = mascot_rows("soya", 0, Depth::True).expect("soya").join("");
        assert!(hoot.contains("38;2;187;154;247"), "hoot is purple");
        assert!(!soya.contains("38;2;187;154;247"));
        assert!(soya.contains("38;2;156;102;68"), "soya is brown");
        assert!(!hoot.contains("38;2;156;102;68"));
        assert!(mascot_rows("nobody", 0, Depth::True).is_none());
    }

    #[test]
    fn test_statusline_mascot_frames_alternate() {
        for name in ["sieve", "soya", "cloud", "sifty", "hoot", "grit"] {
            assert_ne!(
                mascot_rows(name, 0, Depth::True),
                mascot_rows(name, 1, Depth::True),
                "{name}"
            );
        }
    }

    /// The WCAG contrast ratio of two colors.
    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let lin = |v: u8| {
            let c = f64::from(v) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let luma = |c: Rgb| 0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2);
        let (hi, lo) = (luma(a).max(luma(b)), luma(a).min(luma(b)));
        (hi + 0.05) / (lo + 0.05)
    }

    /// The color of a 256-color cube index.
    fn cube_rgb(i: u8) -> Rgb {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let n = (i - 16) as usize;
        (LEVELS[n / 36], LEVELS[n / 6 % 6], LEVELS[n % 6])
    }

    #[test]
    fn test_statusline_text_colors_have_a_contrast_of_4_5_on_the_dark_background() {
        let bg: Rgb = (0x1a, 0x1b, 0x26);
        for c in [SAND, FG_DARK, GREEN, BLUE, TEAL, ORANGE] {
            assert!(contrast(c, bg) >= 4.5, "{c:?} on 24-bit");
            assert!(
                contrast(cube_rgb(nearest_256(c)), bg) >= 4.5,
                "{c:?} on 256"
            );
        }
        assert!(contrast(DIM, bg) < 4.5, "DIM is for the separator only");
    }

    #[test]
    fn test_statusline_format_short_styles() {
        assert_eq!(format_short(950), "950");
        assert_eq!(format_short(1_500), "1.5k");
        assert_eq!(format_short(61_700), "61.7k");
        assert_eq!(format_short(214_000), "214k");
        assert_eq!(format_short(1_250_000), "1.2M");
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sieve-sl-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("make dir");
        dir
    }

    #[test]
    fn test_statusline_colorterm_picks_24_bit_or_256_colors() {
        assert_eq!(depth_from(None, Some("truecolor")), Depth::True);
        assert_eq!(depth_from(None, Some("24bit")), Depth::True);
        assert_eq!(depth_from(None, Some("xterm")), Depth::Ansi256);
        assert_eq!(depth_from(None, None), Depth::Ansi256);
        assert_eq!(depth_from(Some("1"), Some("truecolor")), Depth::Plain);
        assert_eq!(nearest_256(SAND), 179);
        let rgb = mascot_rows("sieve", 0, Depth::True).expect("rows").join("");
        assert!(rgb.contains("38;2;") && !rgb.contains("38;5;"));
        let idx = mascot_rows("sieve", 0, Depth::Ansi256)
            .expect("rows")
            .join("");
        assert!(idx.contains("38;5;") && !idx.contains("38;2;"));
        let stats = fixture_stats();
        let out = render_sieve(&SieveView {
            depth: Depth::Ansi256,
            ..view_of(&stats, None)
        });
        assert!(
            out.contains("\x1b[38;5;179msieve") && !out.contains("38;2;"),
            "{out}"
        );
    }

    #[test]
    fn test_statusline_unknown_mascot_falls_back_to_sieve() {
        let want = mascot_rows("sieve", 0, Depth::True);
        assert!(want.is_some());
        assert_eq!(mascot_for("nobody", 0, Depth::True), want);
        assert_eq!(mascot_for("none", 0, Depth::True), None);
        let dir = scratch_dir("badjson");
        std::fs::write(dir.join("config.json"), "{not json").expect("write config");
        assert_eq!(read_mascot(&dir), "sieve");
        std::fs::write(dir.join("config.json"), r#"{"mascot":7}"#).expect("write config");
        assert_eq!(read_mascot(&dir), "sieve");
        std::fs::remove_dir_all(&dir).expect("remove dir");
    }

    #[test]
    fn test_statusline_frame_follows_now_mod_2() {
        let dir = scratch_dir("frame");
        let at = |now| render_sieve_at(Some(&fixture_stats()), &dir, None, now, Depth::True);
        let (even, odd) = (at(10), at(11));
        std::fs::remove_dir_all(&dir).expect("remove dir");
        let rows = |f| mascot_rows("sieve", f, Depth::True).expect("rows");
        assert!(even.starts_with(&rows(0)[0]), "{even}");
        assert!(odd.starts_with(&rows(1)[0]), "{odd}");
        assert_eq!(frame_for(12), 0);
        assert_eq!(frame_for(13), 1);
    }

    #[test]
    fn test_statusline_missing_stats_keeps_mascot_and_says_no_savings() {
        let out = render_sieve(&SieveView {
            stats: None,
            ..view_of(&fixture_stats(), mascot_rows("sieve", 0, Depth::True))
        });
        let text = strip_ansi(&out);
        let first = text.split('\n').next().unwrap_or("");
        assert!(first.contains('\u{2580}'), "{first}");
        assert!(first.ends_with("sieve \u{2502} no savings yet"), "{first}");
    }

    #[test]
    fn test_statusline_savings_cache_skips_the_session_files() {
        let ctx = scratch_dir("cache");
        let session = ctx.join(".cache").join("session");
        std::fs::create_dir_all(&session).expect("make session dir");
        let reads = std::cell::Cell::new(0);
        let call = |now: u64| {
            cached_savings(&ctx, now, || {
                reads.set(reads.get() + 1);
                (reads.get() as u64, 9)
            })
        };
        assert_eq!(call(100), (1, 9));
        assert_eq!(call(110), (1, 9), "no session change: the cache serves it");
        assert_eq!(reads.get(), 1);
        assert_eq!(call(131), (2, 9), "the cache expires after 30 seconds");
        std::fs::write(session.join("a.json"), "{}").expect("write session");
        assert_eq!(call(132), (3, 9), "a new session file busts the cache");
        assert_eq!(reads.get(), 3);
        std::fs::remove_dir_all(&ctx).expect("remove dir");
    }

    #[test]
    fn test_statusline_mascots_json_ends_with_a_newline() {
        assert!(MASCOTS.ends_with("}\n"));
    }

    #[test]
    fn test_statusline_mascots_json_frames_share_one_size_and_use_palette_chars() {
        let v: Value = serde_json::from_str(MASCOTS).expect("mascots.json parses");
        let palette = v["palette"].as_object().expect("palette");
        assert_eq!(v["default"], "sieve");
        let all = v["mascots"].as_object().expect("mascots");
        assert_eq!(all.len(), 6);
        assert_eq!(v["order"].as_array().map(Vec::len), Some(6));
        // Returns the (rows, cols) of each frame, after the palette check.
        let sizes = |name: &str, key: &str| -> Vec<(usize, usize)> {
            let frames = all[name][key].as_array().expect("frames");
            assert_eq!(frames.len(), 2, "{name} {key}");
            frames
                .iter()
                .map(|f| {
                    let rows = f.as_array().expect("rows");
                    for r in rows.iter().map(|r| r.as_str().expect("row text")) {
                        assert!(
                            r.chars()
                                .all(|c| c == '.' || palette.contains_key(&c.to_string())),
                            "{name}: {r}"
                        );
                        assert_eq!(
                            Some(r.chars().count()),
                            rows[0].as_str().map(|s| s.chars().count()),
                            "{name} ragged"
                        );
                    }
                    (
                        rows.len(),
                        rows[0].as_str().map_or(0, |s| s.chars().count()),
                    )
                })
                .collect()
        };
        for name in all.keys() {
            let large = sizes(name, "frames");
            assert_eq!(
                large[0],
                (14, 16),
                "{name} large frame is 14 rows x 16 cols"
            );
            assert_eq!(large[0], large[1], "{name} frames share one size");
            let small = sizes(name, "small");
            assert_eq!(small[0], small[1], "{name} small frames share one size");
            assert_eq!(small[0].0, 6, "{name} small has 6 rows");
            assert!(matches!(small[0].1, 10 | 12), "{name} small width");
            let tiny = sizes(name, "tiny");
            assert_eq!(tiny[0], tiny[1], "{name} tiny frames share one size");
            assert_eq!(tiny[0].0, 4, "{name} tiny has 4 rows");
            assert!(tiny[0].1 <= 8, "{name} tiny is at most 8 columns wide");
        }
    }

    #[test]
    fn test_statusline_tiny_sieve_cells_match_the_sprite() {
        let v: Value = serde_json::from_str(MASCOTS).expect("mascots.json parses");
        let px = tiny_pixels("sieve", 0, &v).expect("sieve tiny frame");
        let rows = mascot_rows("sieve", 0, Depth::True).expect("rows");
        let fg = |c: Rgb| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2);
        let bg = |c: Rgb| format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2);
        for (i, row) in rows.iter().enumerate() {
            let (top, bottom) = (&px[2 * i], &px[2 * i + 1]);
            let mut rest = row.as_str();
            for (t, b) in top.iter().zip(bottom) {
                let want = match (t, b) {
                    (None, None) => BLANK.to_string(),
                    (Some(t), None) => format!("{}\u{2580}{RESET}", fg(*t)),
                    (None, Some(b)) => format!("{}\u{2584}{RESET}", fg(*b)),
                    (Some(t), Some(b)) => format!("{}{}\u{2580}{RESET}", fg(*t), bg(*b)),
                };
                assert!(
                    rest.starts_with(&want),
                    "row {i}: want {want:?} at {rest:?}"
                );
                rest = &rest[want.len()..];
            }
            assert!(rest.is_empty(), "row {i} has extra cells");
        }
    }

    #[test]
    fn render_subagent_carries_the_agents_last_query() {
        let mut session = SessionState::default();
        session
            .per_agent_query
            .insert("scout".to_string(), "where is run".to_string());
        let out = render_subagent("scout", &session);
        assert!(out.contains("scout"));
        assert!(out.contains("where is run"));
    }

    #[test]
    fn format_thousands_groups_every_three_digits() {
        assert_eq!(format_thousands(420), "420");
        assert_eq!(format_thousands(1_200), "1,200");
    }

    #[test]
    fn test_s0_statusline_skips_wiring_over_cap() {
        hook::set_test_wiring_cap(Some(1));
        let dir = std::env::temp_dir().join(format!("sieve-s0-status-{}", std::process::id()));
        let ctx = hook::resolve_context_dir(&dir);
        std::fs::create_dir_all(ctx.join(".graph")).expect("graph dir");
        std::fs::write(hook::wiring_path(&ctx), "{}").expect("write wiring");
        let stats = resolve_stats(&dir).expect("stats");
        assert!(stats.capped);
        assert_eq!(stats.node_count, 0);
        let text = strip_ansi(&view(&stats, None, false));
        assert!(text.contains("index over the size cap; hooks pass through"));
        assert!(!text.contains("symbols"), "{text}");
        hook::set_test_wiring_cap(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_s0_statusline_uses_stale_counts_without_parse() {
        let dir = std::env::temp_dir().join(format!("sieve-s0-stale-{}", std::process::id()));
        let ctx = hook::resolve_context_dir(&dir);
        std::fs::create_dir_all(ctx.join(".graph")).expect("graph dir");
        std::fs::create_dir_all(ctx.join(".cache")).expect("cache dir");
        // The wiring is garbage: any parse would fail.
        std::fs::write(hook::wiring_path(&ctx), "not json at all").expect("write wiring");
        let counts = serde_json::json!({
            "nodeCount": 752, "edgeCount": 2017, "totalCount": 752, "readyCount": 0,
            "languages": ["rust"], "wiringBytes": 3, "wiringMtimeMs": 0
        });
        std::fs::write(ctx.join(".cache").join("counts.json"), counts.to_string())
            .expect("write counts");
        let stats = resolve_stats(&dir).expect("stats from counts");
        assert!(stats.stale_counts);
        assert_eq!(stats.node_count, 752);
        let text = strip_ansi(&view(&stats, None, false));
        assert!(text.contains("~752 symbols"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
