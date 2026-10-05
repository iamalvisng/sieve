//! Who to tag on a pull request: the people git names on the areas a
//! diff changes or reaches (P1-25, P3-25).
//!
//! One `git log` per area over that area's own files, weighted by
//! recency, ranked, capped. A handle prints only when the commit email
//! is a GitHub noreply address. An owner needs [`MIN_SHARE`] of an area's
//! weight to be named.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;
use serde::Serialize;
use sieve_core::collate::collate;

use super::{run_git, BlastReport, JS_WS};
use crate::callers::js_trim;

/// Recency half-life: a commit 120 days old weighs half a fresh one.
const HALF_LIFE_MS: f64 = 120.0 * 24.0 * 60.0 * 60.0 * 1000.0;
/// History window and commit cap per area.
const SINCE: &str = "--since=36.months";
const MAX_COMMITS: &str = "400";
/// Files passed to one `git log`.
const MAX_PATHSPEC: usize = 80;
/// Share of an area's total weight an owner must hold.
const MIN_SHARE: f64 = 0.15;
/// Names listed per area.
const MAX_PER_AREA: usize = 2;
/// Names in the tag line and the text block.
pub const MAX_REVIEWERS: usize = 3;
/// Weight of an area the diff only reaches.
const AFFECTED_WEIGHT: f64 = 0.6;

/// One person git names on an area's files.
#[derive(Debug, Clone, Serialize)]
pub struct Owner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub commits: u32,
    pub score: f64,
    /// The newest commit's author time, in ms since the epoch.
    pub last: i64,
}

/// One ranked reviewer: an [`Owner`] plus the areas that name them.
#[derive(Debug, Clone, Serialize)]
pub struct Reviewer {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub commits: u32,
    pub score: f64,
    pub last: i64,
    pub areas: Vec<String>,
}

fn handle_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    // JS `/i` without `u` folds ASCII only; JS `\d` is ASCII.
    RE.get_or_init(|| {
        Regex::new(
            r"(?i-u)^(?:[0-9]+\+)?([A-Za-z0-9](?:-?[A-Za-z0-9]){0,38})@users\.noreply\.github\.com$",
        )
        .expect("valid regex")
    })
}

/// A GitHub handle out of a commit email, or `None`.
pub fn github_handle(email: &str) -> Option<String> {
    handle_re()
        .captures(js_trim(email))
        .map(|m| m[1].to_string())
}

fn bot_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i-u)^(github-actions|dependabot|renovate)(\[bot\])?$").expect("valid regex")
    })
}

/// Bots write commits and are never reviewers.
fn is_bot(name: &str, email: &str) -> bool {
    let lower_name = name.to_ascii_lowercase();
    lower_name.contains("[bot]")
        || email.to_ascii_lowercase().contains("[bot]@")
        || bot_re().is_match(js_trim(name))
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Date.parse` of git's `%aI` (`2026-10-01T12:34:56+08:00`), in ms.
pub fn parse_iso_ms(iso: &str) -> Option<i64> {
    let b = iso.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let s = iso.get(from..to)?;
        if !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let year = num(0, 4)?;
    let month = u32::try_from(num(5, 7)?).ok()?;
    let day = u32::try_from(num(8, 10)?).ok()?;
    let hour = num(11, 13)?;
    let minute = num(14, 16)?;
    let second = num(17, 19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let zone = &iso[19..];
    let offset_min = match zone {
        "" | "Z" => 0,
        _ => {
            let sign = match zone.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let zb = zone.as_bytes();
            if zb.len() != 6 || zb[3] != b':' {
                return None;
            }
            let zh: i64 = zone.get(1..3)?.parse().ok()?;
            let zm: i64 = zone.get(4..6)?.parse().ok()?;
            sign * (zh * 60 + zm)
        }
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_min * 60;
    Some(secs * 1000)
}

struct Person {
    name: String,
    handle: Option<String>,
    commits: u32,
    score: f64,
    last: i64,
    seen: HashSet<String>,
}

struct Commit {
    hash: String,
    at: i64,
    weight: f64,
    name: String,
    email: String,
}

/// The people behind `files`, best first.
pub fn owners_for(root: &Path, files: &[String], exclude: &[String], now: i64) -> Vec<Owner> {
    let mut paths: Vec<&str> = files.iter().map(String::as_str).collect();
    paths.sort_unstable();
    paths.truncate(MAX_PATHSPEC);
    if paths.is_empty() {
        return Vec::new();
    }
    let mut args = vec![
        "log",
        "--no-merges",
        SINCE,
        "-n",
        MAX_COMMITS,
        "--format=\x01%H\x02%aI\x02%aN\x02%aE",
        "--name-only",
        "--",
    ];
    args.extend(paths.iter().copied());
    let Some(out) = run_git(root, &args) else {
        return Vec::new();
    };

    let wanted: HashSet<&str> = paths.iter().copied().collect();
    let mut order: Vec<String> = Vec::new();
    let mut people: HashMap<String, Person> = HashMap::new();
    let mut commit: Option<Commit> = None;
    for line in out.split('\n') {
        if let Some(rest) = line.strip_prefix('\x01') {
            let mut parts = rest.split('\x02');
            let hash = parts.next().unwrap_or("").to_string();
            let iso = parts.next().unwrap_or("");
            let name = parts.next().unwrap_or("").to_string();
            let email = parts.next().unwrap_or("").to_string();
            commit = parse_iso_ms(iso).map(|at| Commit {
                hash,
                at,
                weight: 0.5f64.powf((now - at) as f64 / HALF_LIFE_MS),
                name,
                email,
            });
            continue;
        }
        let Some(c) = &commit else { continue };
        if !wanted.contains(line) || is_bot(&c.name, &c.email) {
            continue;
        }
        let handle = github_handle(&c.email);
        let key = handle
            .clone()
            .unwrap_or_else(|| c.email.clone())
            .to_lowercase();
        match people.get_mut(&key) {
            None => {
                people.insert(
                    key.clone(),
                    Person {
                        name: c.name.clone(),
                        handle,
                        commits: 1,
                        score: c.weight,
                        last: c.at,
                        seen: HashSet::from([c.hash.clone()]),
                    },
                );
                order.push(key);
            }
            Some(prev) => {
                // One commit touching six files in the area is one commit.
                if !prev.seen.insert(c.hash.clone()) {
                    continue;
                }
                prev.commits += 1;
                prev.score += c.weight;
                prev.last = prev.last.max(c.at);
            }
        }
    }

    let excluded: HashSet<String> = exclude
        .iter()
        .map(|e| js_trim(e).to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let is_excluded = |key: &str, p: &Person| {
        excluded.contains(key)
            || excluded.contains(&p.name.to_lowercase())
            || p.handle
                .as_ref()
                .is_some_and(|h| excluded.contains(&h.to_lowercase()))
    };
    let mut kept: Vec<Person> = order
        .into_iter()
        .filter_map(|key| {
            let p = people.remove(&key)?;
            (!is_excluded(&key, &p)).then_some(p)
        })
        .collect();
    // The floor is a share of what is left after the author is dropped.
    let total: f64 = kept.iter().map(|p| p.score).sum();
    kept.retain(|p| total == 0.0 || p.score / total >= MIN_SHARE);
    kept.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| collate(&a.name, &b.name))
    });
    kept.truncate(MAX_PER_AREA);
    kept.into_iter()
        .map(|p| Owner {
            name: p.name,
            handle: p.handle,
            commits: p.commits,
            score: p.score,
            last: p.last,
        })
        .collect()
}

fn trailer_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"^(.*?){JS_WS}*<([^>]+)>$")).expect("valid regex"))
}

/// Everyone who authored a commit in `base...HEAD`, co-authors included.
pub fn diff_authors(root: &Path, base: &str) -> Vec<String> {
    let range = format!("{base}...HEAD");
    let Some(out) = run_git(
        root,
        &[
            "log",
            "--format=%aN%n%aE%n%(trailers:key=Co-authored-by,valueonly)",
            &range,
        ],
    ) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !names.iter().any(|n| n == s) {
            names.push(s.to_string());
        }
    };
    for raw in out.split('\n') {
        let line = js_trim(raw);
        if line.is_empty() {
            continue;
        }
        if let Some(m) = trailer_re().captures(line) {
            if !m[1].is_empty() {
                add(&m[1]);
            }
            add(&m[2]);
            continue;
        }
        add(line);
    }
    names
}

/// Whoever git would sign a commit as, here and now.
pub fn local_identity(root: &Path) -> Vec<String> {
    ["user.name", "user.email"]
        .iter()
        .filter_map(|key| run_git(root, &["config", key]))
        .map(|v| js_trim(&v).to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

struct Ranked {
    owner: Owner,
    areas: Vec<String>,
    weighted: f64,
}

/// Fills `owners` on every area and module, and ranks `reviewers`.
/// `now` is ms since the epoch.
pub fn attach_owners(root: &Path, report: &mut BlastReport, exclude: &[String], now: i64) {
    let mut order: Vec<String> = Vec::new();
    let mut ranked: HashMap<String, Ranked> = HashMap::new();
    let mut visit = |label: &str, files: &[String], weight: f64| -> Vec<Owner> {
        let owners = owners_for(root, files, exclude, now);
        for o in &owners {
            let key = o
                .handle
                .clone()
                .unwrap_or_else(|| o.name.clone())
                .to_lowercase();
            match ranked.get_mut(&key) {
                None => {
                    ranked.insert(
                        key.clone(),
                        Ranked {
                            owner: o.clone(),
                            areas: vec![label.to_string()],
                            weighted: o.score * weight,
                        },
                    );
                    order.push(key);
                }
                Some(prev) => {
                    prev.areas.push(label.to_string());
                    prev.owner.commits += o.commits;
                    prev.owner.score += o.score;
                    prev.weighted += o.score * weight;
                    prev.owner.last = prev.owner.last.max(o.last);
                    if prev.owner.handle.is_none() && o.handle.is_some() {
                        prev.owner.handle = o.handle.clone();
                    }
                }
            }
        }
        owners
    };
    for area in &mut report.areas {
        area.owners = Some(visit(&area.label, &area.files, 1.0));
    }
    for module in &mut report.modules {
        module.owners = Some(visit(&module.label, &module.files, AFFECTED_WEIGHT));
    }
    let mut all: Vec<Ranked> = order
        .into_iter()
        .filter_map(|key| ranked.remove(&key))
        .collect();
    all.sort_by(|a, b| {
        b.weighted
            .partial_cmp(&a.weighted)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.owner.commits.cmp(&a.owner.commits))
            .then_with(|| collate(&a.owner.name, &b.owner.name))
    });
    all.truncate(MAX_REVIEWERS);
    report.reviewers = Some(
        all.into_iter()
            .map(|r| Reviewer {
                name: r.owner.name,
                handle: r.owner.handle,
                commits: r.owner.commits,
                score: r.owner.score,
                last: r.owner.last,
                areas: r.areas,
            })
            .collect(),
    );
}

/// `today`, `9d ago`, `3mo ago`, `2y ago`.
pub fn since_label(last: i64, now: i64) -> String {
    let days = ((now - last) as f64 / 86_400_000.0).round().max(0.0) as i64;
    if days < 1 {
        return "today".to_string();
    }
    if days == 1 {
        return "yesterday".to_string();
    }
    if days < 31 {
        return format!("{days}d ago");
    }
    let months = (days as f64 / 30.0).round() as i64;
    if months < 24 {
        format!("{months}mo ago")
    } else {
        format!("{}y ago", (days as f64 / 365.0).round() as i64)
    }
}

/// `YYYY-MM-DD` in UTC for ms since the epoch `isoDay`): the inverse of
/// `days_from_civil`.
pub(super) fn iso_day(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `@handle`, or the bare name when git carries no handle.
pub fn mention(name: &str, handle: Option<&str>) -> String {
    match handle {
        Some(h) => format!("@{h}"),
        None => name.to_string(),
    }
}

/// Now, in ms since the epoch, as `Date.now()`. A test sets
/// `SIEVE_TEST_NOW_MS` to fix the clock, so the owner scores stay stable.
pub fn now_ms() -> i64 {
    if let Some(ms) = std::env::var("SIEVE_TEST_NOW_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
    {
        return ms;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    /// P1-25: git's `%aI` parses to the same ms as `Date.parse`.
    #[test]
    fn test_p1_25_parse_iso_ms_matches_date_parse() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(
            parse_iso_ms("2026-09-01T00:00:00Z"),
            Some(1_788_220_800_000)
        );
        assert_eq!(
            parse_iso_ms("2026-09-01T08:00:00+08:00"),
            Some(1_788_220_800_000)
        );
        assert_eq!(parse_iso_ms("not a date"), None);
    }

    /// P1-25: `sinceLabel` rounds to days, then months, then years.
    #[test]
    fn test_p1_25_since_label_steps() {
        let now = 10_000 * DAY;
        assert_eq!(since_label(now, now), "today");
        assert_eq!(since_label(now - DAY, now), "yesterday");
        assert_eq!(since_label(now - 9 * DAY, now), "9d ago");
        assert_eq!(since_label(now - 100 * DAY, now), "3mo ago");
        assert_eq!(since_label(now - 800 * DAY, now), "2y ago");
        assert_eq!(since_label(now + DAY, now), "today");
    }

    /// P1-25: only a noreply address yields a handle, and bots are dropped.
    #[test]
    fn test_p1_25_handle_and_bot_rules() {
        assert_eq!(
            github_handle(" 123+Some-One@USERS.noreply.github.com "),
            Some("Some-One".to_string())
        );
        assert_eq!(github_handle("someone@example.com"), None);
        assert!(is_bot("dependabot[bot]", "x@y"));
        assert!(is_bot("Renovate", "x@y"));
        assert!(!is_bot("Other Person", "other@example.com"));
    }

    /// P1-25: a co-author trailer yields both its halves.
    #[test]
    fn test_p1_25_trailer_re_splits_name_and_email() {
        let m = trailer_re()
            .captures("Pair Person <pair@example.com>")
            .expect("matches");
        assert_eq!(&m[1], "Pair Person");
        assert_eq!(&m[2], "pair@example.com");
    }
}
