//! `sieve statusline`: the Claude Code statusline (the `hosts-hooks.md` note
//! section 4).

use std::io::Read as _;
use std::path::Path;

use clap::Args;
use serde_json::Value;
use sieve_core::product::product;
use sieve_core::wiring::{Graph, SummaryState};

use crate::hook::{self, SessionState, Stats};
use crate::hosts;

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
        render_sieve_now(
            stats.as_ref(),
            &project_dir,
            ctx_pct,
            session.current_saved()
        )
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
    /// The tokens this session saved. 0 means none yet.
    session: u64,
    today: u64,
    /// The seven mascot rows, or `None` for no mascot.
    mascot: Option<Vec<String>>,
    depth: Depth,
    /// The note shown when the index is over the size cap.
    cap_note: &'static str,
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

type Pixels = Vec<Vec<Option<Rgb>>>;

const WHITE: Rgb = (255, 255, 255);

/// The darkest palette color, by the sum of r, g and b.
fn darkest(palette: &serde_json::Map<String, Value>) -> Option<Rgb> {
    palette
        .values()
        .filter_map(|v| parse_hex(v.as_str()?))
        .min_by_key(|c| u32::from(c.0) + u32::from(c.1) + u32::from(c.2))
}

fn pixel_at(px: &Pixels, y: usize, x: usize) -> Option<Rgb> {
    px.get(y).and_then(|r| r.get(x)).copied().flatten()
}

/// The 4 neighbours of a pixel. A step off the top or the left wraps to a
/// huge index, which `pixel_at` reads as empty.
fn near4(y: usize, x: usize) -> [(usize, usize); 4] {
    [
        (y.wrapping_sub(1), x),
        (y + 1, x),
        (y, x.wrapping_sub(1)),
        (y, x + 1),
    ]
}

/// The eyes of a face: each 4-connected group of darkest or white pixels that
/// holds a darkest pixel next to a white pixel.
fn eye_groups(px: &Pixels, dark: Rgb) -> Vec<Vec<(usize, usize)>> {
    let eye = |y, x| pixel_at(px, y, x).is_some_and(|c| c == dark || c == WHITE);
    let mut seen = std::collections::HashSet::new();
    let mut groups = Vec::new();
    for (y, row) in px.iter().enumerate() {
        for x in 0..row.len() {
            if !eye(y, x) || !seen.insert((y, x)) {
                continue;
            }
            let mut group = vec![(y, x)];
            let mut i = 0;
            while i < group.len() {
                let (cy, cx) = group[i];
                i += 1;
                for n in near4(cy, cx) {
                    if eye(n.0, n.1) && seen.insert(n) {
                        group.push(n);
                    }
                }
            }
            let pair = group.iter().any(|&(gy, gx)| {
                pixel_at(px, gy, gx) == Some(dark)
                    && near4(gy, gx)
                        .iter()
                        .any(|&(ny, nx)| pixel_at(px, ny, nx) == Some(WHITE))
            });
            if pair {
                groups.push(group);
            }
        }
    }
    groups
}

/// The blink art: each eye group takes the most common color of its 4-neighbour
/// border, then the middle row of its bounding box is a lid of the darkest
/// color over the group's columns.
fn blink_art(px: &Pixels, dark: Rgb) -> Pixels {
    let mut out = px.clone();
    for group in eye_groups(px, dark) {
        let mut count: Vec<(Rgb, usize)> = Vec::new();
        for &(y, x) in &group {
            for (ny, nx) in near4(y, x) {
                let Some(c) = pixel_at(px, ny, nx) else {
                    continue;
                };
                if group.contains(&(ny, nx)) {
                    continue;
                }
                match count.iter_mut().find(|e| e.0 == c) {
                    Some(e) => e.1 += 1,
                    None => count.push((c, 1)),
                }
            }
        }
        // A tie takes the color seen first.
        let best = count.iter().fold(None::<&(Rgb, usize)>, |b, e| match b {
            Some(b) if b.1 >= e.1 => Some(b),
            _ => Some(e),
        });
        if let Some(&(c, _)) = best {
            for &(y, x) in &group {
                out[y][x] = Some(c);
            }
        }
        let top = group.iter().map(|g| g.0).min().unwrap_or(0);
        let bottom = group.iter().map(|g| g.0).max().unwrap_or(0);
        for &(_, x) in &group {
            out[(top + bottom) / 2][x] = Some(dark);
        }
    }
    out
}

/// The first row of the 8-row face. With eyes, it starts 2 rows above the
/// highest eye. Without eyes, it starts the window with the most filled pixels.
fn face_top(px: &Pixels, dark: Rgb) -> usize {
    let last = px.len().saturating_sub(8);
    let top_eye = eye_groups(px, dark).iter().flatten().map(|g| g.0).min();
    if let Some(y) = top_eye {
        return y.saturating_sub(2).min(last);
    }
    let filled = |t: usize| {
        px[t..t + 8]
            .iter()
            .map(|r| r.iter().flatten().count())
            .sum::<usize>()
    };
    (0..=last).fold(0, |best, t| if filled(t) > filled(best) { t } else { best })
}

/// The face rows of one mascot from frame 0 of its 14-row art, after the crop
/// rule and the empty column trim. The eye groups and the crop rows are
/// returned with the pixels. `blink` paints the closed lids.
fn face_of(name: &str, blink: bool, mascots: &Value) -> Option<(Pixels, usize)> {
    let rows = mascots
        .get("mascots")?
        .get(name)?
        .get("frames")?
        .get(0)?
        .as_array()?;
    let palette = mascots.get("palette")?.as_object()?;
    let dark = darkest(palette)?;
    let color_of = |c: char| parse_hex(palette.get(c.to_string().as_str())?.as_str()?);
    let px: Pixels = rows
        .iter()
        .filter_map(Value::as_str)
        .map(|r| r.chars().map(color_of).collect())
        .collect();
    if px.len() != 14 {
        return None;
    }
    let top = face_top(&px, dark);
    let art = if blink {
        blink_art(&px, dark)
    } else {
        px.clone()
    };
    let open = &px[top..top + 8];
    let used = |x: usize| open.iter().any(|r| r.get(x).is_some_and(Option::is_some));
    let width = open.iter().map(Vec::len).max()?;
    let lo = (0..width).find(|&x| used(x))?;
    let hi = (0..width).rfind(|&x| used(x))?;
    let face = art[top..top + 8]
        .iter()
        .map(|r| r[lo..=hi.min(r.len() - 1)].to_vec())
        .collect();
    Some((face, top))
}

/// Reads the face. `frame` 0 is the open face. `frame` 1 is the blink frame.
/// A face with no eye group does not blink.
fn frame_pixels(name: &str, frame: usize, mascots: &Value) -> Option<Pixels> {
    face_of(name, frame != 0, mascots).map(|f| f.0)
}

/// The eye groups of the open face of one mascot, in face coordinates.
#[cfg(test)]
fn eye_list(name: &str, mascots: &Value) -> Vec<Vec<(usize, usize)>> {
    let open = frame_pixels(name, 0, mascots).expect("face");
    let dark = darkest(mascots["palette"].as_object().expect("palette")).expect("dark");
    eye_groups(&open, dark)
}

/// The width of the widest face over all mascots. Every mascot row is padded
/// to this width, so the text column never moves.
fn face_width(mascots: &Value) -> usize {
    let Some(all) = mascots.get("mascots").and_then(Value::as_object) else {
        return 0;
    };
    all.keys()
        .filter_map(|n| frame_pixels(n, 0, mascots))
        .filter_map(|p| p.first().map(Vec::len))
        .max()
        .unwrap_or(0)
}

/// U+2800 BRAILLE PATTERN BLANK. It is one column wide and is not whitespace,
/// so Claude Code does not trim it from the start of a status line.
const BLANK: &str = "\u{2800}";

/// Draws the four terminal rows of the mascot face. A cell with a top
/// and a bottom pixel is an upper half block: the top color is the
/// foreground and the bottom color is the background. A cell with one
/// pixel is an upper or a lower half block.
fn mascot_rows(name: &str, frame: usize, depth: Depth) -> Option<Vec<String>> {
    let all = mascots()?;
    let px = frame_pixels(name, frame, all)?;
    let pad = face_width(all).saturating_sub(px[0].len());
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
            .chain(std::iter::repeat_n(BLANK.to_string(), pad))
            .collect()
    };
    Some(px.chunks(2).map(|p| row(&p[0], &p[1])).collect())
}

/// The mascot rows for a config name. `none` gives no mascot. An unknown
/// name gives the default `sieve` mascot.
fn mascot_for(name: &str, frame: usize, depth: Depth) -> Option<Vec<String>> {
    if name == "none" || depth == Depth::Plain {
        return None;
    }
    mascot_rows(name, frame, depth).or_else(|| mascot_rows("sieve", frame, depth))
}

/// The blink frame shows for 1 refresh out of every 4 of the clock.
fn frame_for(now: u64) -> usize {
    usize::from(now % 4 == 3)
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
fn render_sieve_now(
    stats: Option<&Stats>,
    project_dir: &Path,
    ctx_pct: Option<i64>,
    session: u64,
) -> String {
    let depth = depth_from(
        std::env::var("NO_COLOR").ok().as_deref(),
        std::env::var("COLORTERM").ok().as_deref(),
    );
    let now = crate::telemetry::now_secs();
    let text = render_sieve_at(stats, project_dir, ctx_pct, session, now, depth);
    let note = setup_note(project_dir, now);
    if note.is_empty() {
        return text;
    }
    // The note goes at the end of line 2, like the cap note.
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    if let Some(line2) = lines.get_mut(1) {
        line2.push_str(&note);
    }
    lines.join("\n")
}

/// The seconds a setup check result stays valid.
const SETUP_TTL_SECS: u64 = 60;

/// The short note for a repo file with an old hook timeout.
const SETUP_NOTE_REPO: &str = " \u{b7} old hook timeouts; run `sieve init`";

/// The short note when a file under `HOME` has an old hook timeout.
const SETUP_NOTE_GLOBAL: &str = " \u{b7} old hook timeouts; run `sieve init --global`";

/// The cache file of the setup check.
fn setup_cache_path(context_dir: &Path) -> std::path::PathBuf {
    context_dir.join(".cache").join("statusline-setup.json")
}

/// Deletes the setup check cache, so the next status line checks again.
pub fn clear_setup_cache(context_dir: &Path) {
    // A missing file is fine: there is nothing to clear.
    let _ = std::fs::remove_file(setup_cache_path(context_dir));
}

/// Returns the setup note text, or an empty string for no note. The check
/// reads up to three settings files, so the result is cached.
fn setup_note(project_dir: &Path, now: u64) -> String {
    let context_dir = crate::stats::stats_context_dir(project_dir);
    cached_setup_note(&context_dir, now, || {
        match hosts::setup_notes(project_dir).first() {
            None => String::new(),
            Some(n) if n.global => SETUP_NOTE_GLOBAL.to_string(),
            Some(_) => SETUP_NOTE_REPO.to_string(),
        }
    })
}

/// Serves the cached setup check while it is under `SETUP_TTL_SECS` old.
/// Else `compute` runs and the cache is rewritten.
fn cached_setup_note(context_dir: &Path, now: u64, compute: impl FnOnce() -> String) -> String {
    let path = setup_cache_path(context_dir);
    let hit = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| {
            let at = v.get("at").and_then(Value::as_u64).unwrap_or(0);
            now.saturating_sub(at) < SETUP_TTL_SECS
        })
        .and_then(|v| v.get("note").and_then(Value::as_str).map(String::from));
    if let Some(note) = hit {
        return note;
    }
    let note = compute();
    // A failed write only costs a recheck on the next run.
    let _ = std::fs::write(
        &path,
        serde_json::json!({"at": now, "note": &note}).to_string(),
    );
    note
}

/// The note for an index over the size cap, with no valid lookup.
const CAP_NOTE_PASS: &str = " \u{b7} index over the size cap; hooks pass through";

/// The note for an index over the size cap. A valid lookup keeps the hooks
/// working, so only the prompt context and the MCP tools are off (E7).
fn cap_note_text(project_dir: &Path) -> &'static str {
    let context_dir = hook::resolve_context_dir(project_dir);
    if hook::open_lookup(&context_dir).is_some() {
        " \u{b7} index over the size cap; ask and MCP off"
    } else {
        CAP_NOTE_PASS
    }
}

/// Renders the sieve status line for a given time and color depth.
fn render_sieve_at(
    stats: Option<&Stats>,
    project_dir: &Path,
    ctx_pct: Option<i64>,
    session: u64,
    now: u64,
    depth: Depth,
) -> String {
    let name = read_mascot(&project_dir.join(product().home_dir_name()));
    let (today, _week) = read_savings(project_dir, now);
    render_sieve(&SieveView {
        stats,
        ctx_pct,
        session,
        today,
        mascot: mascot_for(&name, frame_for(now), depth),
        depth,
        cap_note: if stats.is_some_and(|s| s.capped) {
            cap_note_text(project_dir)
        } else {
            CAP_NOTE_PASS
        },
    })
}

/// Renders the sieve status line: 4 rows with a mascot, else the text rows only.
fn render_sieve(v: &SieveView) -> String {
    let paint = |c: Rgb, s: &str| {
        if v.depth == Depth::Plain {
            s.to_string()
        } else {
            format!("{}{s}{RESET}", code(38, c, v.depth))
        }
    };
    // Each mascot row has the same width, so the text column lines up.
    let cell = |i: usize| match &v.mascot {
        Some(rows) => format!("{}{BLANK}", rows[i]),
        None => String::new(),
    };
    let (cells1, cells2, cells3) = (cell(0), cell(1), cell(2));
    let head = format!("{cells1}{}{}", paint(SAND, "sieve"), paint(DIM, " \u{b7} "));
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
                paint(ORANGE, v.cap_note)
            } else {
                String::new()
            };
            (
                format!(
                    "{head}{}",
                    paint(
                        GREEN,
                        &if v.session > 0 {
                            format!(
                                "\u{1f3af} ~{} this session \u{b7} ~{} today",
                                format_short(v.session),
                                format_short(v.today)
                            )
                        } else {
                            format!("\u{1f3af} ~{} today", format_short(v.today))
                        }
                    ),
                ),
                format!("{cells2}{}{fresh}{cap_note}", paint(FG_DARK, &counts)),
            )
        }
    };
    let mut lines = vec![line1, line2];
    // Line 3 shows the ctx text, or only the mascot cells when there is no ctx.
    let ctx = v.ctx_pct.map(|pct| paint(FG_DARK, &format!("ctx {pct}%")));
    if v.mascot.is_some() || ctx.is_some() {
        lines.push(format!("{cells3}{}", ctx.unwrap_or_default()));
    }
    // Row 4 holds only the mascot.
    if let Some(rows) = &v.mascot {
        lines.extend(rows[3..].iter().cloned());
    }
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

    fn view_of(stats: &Stats, mascot: Option<Vec<String>>) -> SieveView<'_> {
        SieveView {
            stats: Some(stats),
            ctx_pct: Some(54),
            session: 1_500,
            today: 61_700,
            mascot,
            depth: Depth::True,
            cap_note: CAP_NOTE_PASS,
        }
    }

    fn view(stats: &Stats, mascot: Option<Vec<String>>, color: bool) -> String {
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
        assert_eq!(lines.len(), 4, "{text}");
        // The face is `w` cells wide, then one blank, then the text.
        let w = face_width(mascots().expect("mascots"));
        let cut = |l: &str, n: usize| l.chars().skip(n).collect::<String>();
        for l in &lines {
            let cells: String = l.chars().take(w).collect();
            assert!(
                cells
                    .chars()
                    .all(|c| "\u{2800}\u{2580}\u{2584}".contains(c)),
                "{l}"
            );
            if l.chars().count() > w {
                assert_eq!(l.chars().nth(w), Some('\u{2800}'), "{l}");
            }
        }
        assert!(lines[0].contains('\u{2580}'), "{}", lines[0]);
        assert_eq!(
            cut(lines[0], w + 1),
            "sieve \u{b7} \u{1f3af} ~1.5k this session \u{b7} ~61.7k today"
        );
        assert_eq!(
            cut(lines[1], w + 1),
            "752 symbols \u{b7} 2,017 links \u{b7} \u{2713} up to date"
        );
        assert_eq!(cut(lines[2], w + 1), "ctx 54%");
        let no_ctx = strip_ansi(&render_sieve(&SieveView {
            ctx_pct: None,
            ..view_of(&stats, mascot_rows("sieve", 0, Depth::True))
        }));
        assert_eq!(no_ctx.split('\n').count(), 4, "the mascot keeps 4 rows");
        let no_mascot = strip_ansi(&render_sieve(&SieveView {
            ctx_pct: None,
            ..view_of(&stats, None)
        }));
        assert_eq!(no_mascot.split('\n').count(), 2);
        assert!(out.contains("\x1b[48;2;"), "bottom pixels use a background");
    }

    #[test]
    fn test_statusline_mascot_survives_a_leading_whitespace_trim() {
        let stats = fixture_stats();
        let text = strip_ansi(&view(&stats, mascot_rows("sieve", 0, Depth::True), true));
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 4);
        let w = face_width(mascots().expect("mascots"));
        assert!(lines[2].ends_with("ctx 54%"), "line 3 has the ctx text");
        // `trim_start` removes Unicode White_Space, as JS trimStart does.
        let col = |l: &str, w: &str| l.find(w).map(|i| l[..i].chars().count());
        for (l, word) in lines.iter().zip(["sieve", "752"]) {
            assert!(!l.chars().next().is_some_and(char::is_whitespace), "{l}");
            assert_eq!(*l, l.trim_start(), "the trim removed cells");
            assert_eq!(col(l, word), Some(w + 1), "{l}");
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
            "sieve \u{b7} \u{1f3af} ~1.5k this session \u{b7} ~61.7k today\n\
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
        // The sieve frames differ only below the face, so sieve does not blink.
        for name in ["soya", "cloud", "sifty", "hoot", "grit"] {
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
        for c in [SAND, FG_DARK, GREEN, TEAL, ORANGE] {
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
    fn test_statusline_frame_follows_now_mod_4() {
        assert_eq!(frame_for(12), 0);
        assert_eq!(frame_for(13), 0);
        assert_eq!(frame_for(14), 0);
        assert_eq!(frame_for(15), 1);
    }

    #[test]
    fn test_statusline_blink_draws_a_lid_row_in_each_eye_group() {
        let v = mascots().expect("mascots");
        let dark = darkest(v["palette"].as_object().expect("palette")).expect("dark");
        for name in ["soya", "cloud", "sifty", "hoot", "grit"] {
            let blink = frame_pixels(name, 1, v).expect("blink");
            for g in eye_list(name, v) {
                let top = g.iter().map(|p| p.0).min().expect("top");
                let bottom = g.iter().map(|p| p.0).max().expect("bottom");
                for &(_, x) in &g {
                    assert_eq!(blink[(top + bottom) / 2][x], Some(dark), "{name} lid");
                }
            }
        }
    }

    #[test]
    fn test_statusline_every_mascot_has_a_stable_crop_and_a_generic_blink() {
        let v = mascots().expect("mascots");
        let w = face_width(v);
        for name in ["sieve", "soya", "cloud", "sifty", "hoot", "grit"] {
            let open = frame_pixels(name, 0, v).expect("open");
            let blink = frame_pixels(name, 1, v).expect("blink");
            let rows = mascot_rows(name, 0, Depth::True).expect("rows");
            assert_eq!(rows.len(), 4, "{name}");
            assert_eq!(open[0].len(), blink[0].len(), "{name} width");
            assert!(open[0].len() <= w, "{name}");
            let eyes = eye_list(name, v);
            if name == "sieve" {
                assert!(eyes.is_empty(), "sieve has no eyes");
                assert_eq!(open, blink, "sieve does not blink");
                continue;
            }
            assert!(!eyes.is_empty(), "{name} has eyes");
            let inside: Vec<_> = eyes.iter().flatten().collect();
            let mut changed = 0;
            for (y, x) in (0..8).flat_map(|y| (0..open[0].len()).map(move |x| (y, x))) {
                if open[y][x] != blink[y][x] {
                    changed += 1;
                    assert!(inside.contains(&&(y, x)), "{name} changed ({y},{x})");
                }
            }
            assert!(changed > 0, "{name} blink differs");
        }
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
        assert!(first.ends_with("sieve \u{b7} no savings yet"), "{first}");
    }

    #[test]
    fn test_statusline_setup_check_runs_once_per_minute() {
        let ctx = scratch_dir("setup");
        std::fs::create_dir_all(ctx.join(".cache")).expect("cache dir");
        let calls = std::cell::Cell::new(0);
        let call = |now: u64| {
            cached_setup_note(&ctx, now, || {
                calls.set(calls.get() + 1);
                SETUP_NOTE_GLOBAL.to_string()
            })
        };
        assert_eq!(call(100), SETUP_NOTE_GLOBAL);
        assert_eq!(call(159), SETUP_NOTE_GLOBAL);
        assert_eq!(calls.get(), 1, "the cache serves the second call");
        call(160);
        assert_eq!(calls.get(), 2, "the cache expires after 60 seconds");
        clear_setup_cache(&ctx);
        assert!(!setup_cache_path(&ctx).exists(), "the cache is cleared");
        clear_setup_cache(&ctx);
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
            let status = sizes(name, "status");
            assert_eq!(status[0], status[1], "{name} status frames share one size");
            assert_eq!(status[0], (6, 12), "{name} status is 6 rows x 12 columns");
        }
    }

    #[test]
    fn test_statusline_frame_sieve_cells_match_the_sprite() {
        let v: Value = serde_json::from_str(MASCOTS).expect("mascots.json parses");
        let px = frame_pixels("sieve", 0, &v).expect("sieve frame");
        let rows = mascot_rows("sieve", 0, Depth::True).expect("rows");
        let fg = |c: Rgb| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2);
        let bg = |c: Rgb| format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2);
        assert_eq!((rows.len(), px.len()), (4, 8), "the face is 8 pixel rows");
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
            assert!(
                rest.chars().all(|c| c == '\u{2800}'),
                "row {i} has extra cells"
            );
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

    #[test]
    fn test_s3_statusline_capped_text() {
        let dir = scratch_dir("s3-cap-text");
        std::fs::write(dir.join("a.ts"), "export function a() {}\n").expect("write source");
        let ctx = hook::resolve_context_dir(&dir);
        sieve_parse::rebuild_graph_only(&dir, &ctx).expect("build graph");
        let stats = Stats {
            node_count: 3,
            capped: true,
            ..Stats::default()
        };
        let text = |_: ()| {
            strip_ansi(&render_sieve_at(
                Some(&stats),
                &dir,
                None,
                0,
                0,
                Depth::Plain,
            ))
        };
        let with = text(());
        assert!(
            with.contains("index over the size cap; ask and MCP off"),
            "{with}"
        );
        hook::s3_support::drop_lookup(&ctx);
        let without = text(());
        assert!(
            without.contains("index over the size cap; hooks pass through"),
            "{without}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
