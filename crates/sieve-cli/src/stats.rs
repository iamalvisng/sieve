//! The `sieve stats [dir] [--json]` subcommand (P1-64): the latest session
//! file, the session text readout, and the dollars saved at the session
//! billed input rate.
//!
//! The command reads `<context_dir>/.cache/session/*.json` and nothing
//! else: no graph, no network, no telemetry.

use std::path::{Path, PathBuf};

use clap::Args;
use sieve_core::product::product;

use crate::jsonv::Json;
use crate::query;
use crate::ui::Ui;

/// Flags for `sieve stats`.
#[derive(Args, Debug)]
pub struct StatsArgs {
    /// repository root (default: nearest ancestor with a sieve/ index)
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,
    /// output the session stats as JSON
    #[arg(long)]
    pub json: bool,
}

/// The context dir `stats` reads sessions under: `SIEVE_DIR` (absolute, or
/// relative to `root`) or `<root>/sieve`. The global `--dir` flag does not
/// reach it.
pub(crate) fn stats_context_dir(root: &Path) -> PathBuf {
    let p = product();
    match p.env("DIR").filter(|v| !v.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                root.join(dir)
            }
        }
        None => root.join(p.context_dir_name()),
    }
}

/// The session shape `readSession` falls back to for a missing or
/// unparseable file.
fn empty_session() -> Vec<(String, Json)> {
    Json::obj(vec![
        ("lastQuery", Json::Null),
        ("perAgentQuery", Json::obj(vec![])),
        ("toolReads", Json::n(0)),
        ("sourceReads", Json::n(0)),
        ("savedTokens", Json::n(0)),
        ("injectedPointers", Json::Arr(vec![])),
        ("nudges", Json::n(0)),
    ])
    .pairs()
    .to_vec()
}

/// The newest session file's contents, with the session id in front
/// (`latestSession`). `None` when no `*.json` file exists.
pub fn latest_session(root: &Path) -> Option<Json> {
    let path = sieve_core::session::latest_session_path(&stats_context_dir(root))?;
    let id = path.file_stem()?.to_str()?.to_string();
    // `readJson(...) ?? emptySession()`: a `null` literal also falls back.
    let file = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| Json::parse(&t))
        .filter(|v| *v != Json::Null);
    // `{ id, ...session }`: the spread keeps `id` in front, and a
    // non-object spreads to nothing worth keeping.
    let mut pairs = vec![("id".to_string(), Json::s(id))];
    let extra = match file {
        Some(v) => v.pairs().to_vec(),
        None => empty_session(),
    };
    for (k, v) in extra {
        Json::set(&mut pairs, &k, v);
    }
    Some(Json::Obj(pairs))
}

/// `s.field ?? 0`, then read as a number. A non-number reads as 0.
fn num(s: &Json, key: &str) -> f64 {
    s.get(key).and_then(Json::as_f64).unwrap_or(0.0)
}

/// `Number.prototype.toLocaleString()` in the `en-US` locale: thousands
/// groups, at most 3 fraction digits.
fn to_locale_string(v: f64) -> String {
    let rounded = (v * 1000.0).round() / 1000.0;
    let text = format!("{rounded:.3}");
    let (int_part, frac) = text.split_once('.').unwrap_or((&text, ""));
    let (sign, digits) = match int_part.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("", int_part),
    };
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac}")
    }
}

/// `dollarsSaved`: the saving at the session's blended rate, or `None`
/// when no turn was billed or nothing was saved.
pub(crate) fn dollars_saved(saved: f64, cost: f64, billed: f64) -> Option<f64> {
    if cost == 0.0 || billed == 0.0 || saved <= 0.0 {
        return None;
    }
    let usd = (saved * (cost / billed)) / 1_000_000.0;
    usd.is_finite().then_some(usd)
}

/// `formatDollars`: `$1.23`, or `<$0.01` for a sub-cent amount.
pub(crate) fn format_dollars(usd: f64) -> String {
    if usd < 0.01 {
        "<$0.01".to_string()
    } else {
        format!("${usd:.2}")
    }
}

/// Formats a token count as `950`, `1.5k`, `61.7k`, `214k` or `1.2M`.
fn short_count(n: u64) -> String {
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

/// The text readout: what sieve saved today and this week, then the
/// session and its last query when a session exists.
pub fn format_stats(ui: &Ui, session: Option<&Json>, today: u64, week: u64) -> String {
    let name = product().name;
    let mut lines: Vec<String> = Vec::new();
    if week == 0 {
        lines.push(ui.dim(&format!(
            "no saved tokens yet \u{2014} they appear after an agent session uses {name}"
        )));
    } else {
        lines.push(format!(
            "{} {} {} {} {} {}",
            ui.fg(&format!("{name} saved")),
            ui.green(&short_count(today)),
            ui.fg("tokens today"),
            ui.dim("\u{b7}"),
            ui.green(&short_count(week)),
            ui.fg("this week"),
        ));
    }
    let Some(s) = session else {
        return lines.join("\n");
    };
    let reads = num(s, "toolReads");
    let source = num(s, "sourceReads");
    let saved = num(s, "savedTokens");
    if reads + source + saved > 0.0 {
        let mix = if reads + source == 0.0 {
            "no retrieval yet".to_string()
        } else {
            format!(
                "{}% of reads through {name}",
                ((reads / (reads + source)) * 100.0).round()
            )
        };
        let value = dollars_saved(
            saved,
            num(s, "inputCostMicros"),
            num(s, "inputTokensBilled"),
        )
        .map(|usd| format!(" \u{b7} ~{}", format_dollars(usd)))
        .unwrap_or_default();
        lines.push(format!(
            "{} {}",
            ui.fg("this session"),
            ui.dim(&format!(
                "{} tokens saved \u{b7} {mix}{value}",
                to_locale_string(saved)
            ))
        ));
    }
    if let Some(q) = s
        .get("lastQuery")
        .and_then(Json::as_str)
        .filter(|q| !q.is_empty())
    {
        lines.push(format!("{} {}", ui.fg("last query"), ui.dim(q)));
    }
    lines.join("\n")
}

/// A dollar amount as a JSON number rounded to four places, or `null`.
fn json_usd(usd: Option<f64>) -> Json {
    usd.and_then(|v| serde_json::Number::from_f64((v * 10_000.0).round() / 10_000.0))
        .map_or(Json::Null, Json::Num)
}

/// The tokens and dollars of one day or session as a JSON object.
fn totals_obj(head: (&str, Json), tokens: u64, usd: Option<f64>) -> Json {
    Json::obj(vec![
        head,
        ("tokens", Json::n(tokens as i64)),
        ("dollars", json_usd(usd)),
    ])
}

/// The sieve savings report: this session, today and the last 7 days.
///
/// It reads `<context_dir>/.cache/session/*.json`. Each file holds a
/// `savedByDay` map of UTC date to tokens. A session prices its tokens at
/// its own blended input rate, `inputCostMicros / inputTokensBilled`. A
/// session with no billed turn adds tokens and no dollars. `now_secs` is
/// the Unix time of the report. `offset_secs` is the local UTC offset, so
/// a day is a local day. `latest` is the newest session.
pub fn savings_report(
    context_dir: &Path,
    latest: Option<&Json>,
    now_secs: u64,
    offset_secs: i64,
) -> Vec<(String, Json)> {
    let mut by_day: std::collections::BTreeMap<String, (u64, Option<f64>)> = Default::default();
    let dir = context_dir.join(".cache").join("session");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(file) = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|t| Json::parse(&t))
        else {
            continue;
        };
        let cost = num(&file, "inputCostMicros");
        let billed = num(&file, "inputTokensBilled");
        for (day, tokens) in file.get("savedByDay").map_or(&[][..], Json::pairs) {
            let tokens = tokens.as_f64().unwrap_or(0.0);
            let slot = by_day.entry(day.clone()).or_insert((0, None));
            slot.0 += tokens as u64;
            if let Some(usd) = dollars_saved(tokens, cost, billed) {
                slot.1 = Some(slot.1.unwrap_or(0.0) + usd);
            }
        }
    }
    let day_obj = |date: String| {
        let (tokens, usd) = by_day.get(&date).copied().unwrap_or((0, None));
        totals_obj(("date", Json::s(date)), tokens, usd)
    };
    let (id, tokens, usd) = match latest {
        Some(s) => (
            s.get("id").cloned().unwrap_or(Json::Null),
            num(s, "savedTokens"),
            dollars_saved(
                num(s, "savedTokens"),
                num(s, "inputCostMicros"),
                num(s, "inputTokensBilled"),
            ),
        ),
        None => (Json::Null, 0.0, None),
    };
    let dates: Vec<String> = (0..7u64)
        .map(|i| crate::telemetry::local_date(now_secs.saturating_sub(i * 86_400), offset_secs))
        .collect();
    let (mut total, mut total_usd) = (0u64, None::<f64>);
    for (t, u) in dates.iter().filter_map(|d| by_day.get(d)) {
        total += t;
        if let Some(u) = u {
            total_usd = Some(total_usd.unwrap_or(0.0) + u);
        }
    }
    let days = dates.iter().map(|d| day_obj(d.clone())).collect();
    vec![
        ("session".to_string(), totals_obj(("id", id), tokens as u64, usd)),
        (
            "today".to_string(),
            day_obj(crate::telemetry::local_date(now_secs, offset_secs)),
        ),
        ("last7days".to_string(), Json::Arr(days)),
        (
            "last7daysTotal".to_string(),
            Json::obj(vec![
                ("tokens", Json::n(total as i64)),
                ("dollars", json_usd(total_usd)),
            ]),
        ),
        (
            "basis".to_string(),
            Json::s("input tokens at the session blended billed input rate (inputCostMicros / inputTokensBilled)"),
        ),
    ]
}

/// Runs `sieve stats`: the root walk, then the readout.
pub fn run(args: &StatsArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let root = query::query_root(args.dir.as_deref(), dir_override, &cwd);
    let session = latest_session(&root);
    if args.json {
        let now = crate::telemetry::now_secs();
        let mut pairs = session
            .as_ref()
            .map_or_else(Vec::new, |s| s.pairs().to_vec());
        for (k, v) in savings_report(
            &stats_context_dir(&root),
            session.as_ref(),
            now,
            crate::telemetry::local_offset_secs(),
        ) {
            Json::set(&mut pairs, &k, v);
        }
        println!("{}", Json::Obj(pairs).to_pretty());
        return Ok(());
    }
    if args.json {
        println!(
            "{}",
            session
                .as_ref()
                .map_or_else(|| "null".to_string(), Json::to_pretty)
        );
        return Ok(());
    }
    let report = savings_report(
        &stats_context_dir(&root),
        session.as_ref(),
        crate::telemetry::now_secs(),
        crate::telemetry::local_offset_secs(),
    );
    let tokens = |key: &str| -> u64 {
        report
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.get("tokens"))
            .and_then(Json::as_f64)
            .unwrap_or(0.0) as u64
    };
    println!(
        "{}",
        format_stats(
            &Ui::stdout(),
            session.as_ref(),
            tokens("today"),
            tokens("last7daysTotal")
        )
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "sieve-cli-stats-unit-{label}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("sieve/.cache/session")).expect("create scratch");
            Scratch(path)
        }

        fn write(&self, name: &str, body: &str) {
            fs::write(self.0.join("sieve/.cache/session").join(name), body).expect("write");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_p1_64_no_session_line_and_null_json() {
        let s = Scratch::new("none");
        assert_eq!(latest_session(&s.0), None);
        assert_eq!(
            format_stats(&Ui::plain(), None, 0, 0),
            "no saved tokens yet \u{2014} they appear after an agent session uses sieve"
        );
    }

    #[test]
    fn test_p1_64_one_session_text_and_json_keep_file_order() {
        let s = Scratch::new("one");
        s.write(
            "x.json",
            r#"{"lastQuery":"ask bigwidget","perAgentQuery":{},"toolReads":3,"sourceReads":1,"savedTokens":12345,"injectedPointers":[],"nudges":0,"inputCostMicros":5000000,"inputTokensBilled":1000000}"#,
        );
        let session = latest_session(&s.0).expect("session");
        assert_eq!(
            format_stats(&Ui::plain(), Some(&session), 123_456, 140_200),
            "sieve saved 123k tokens today \u{b7} 140k this week\n\
             this session 12,345 tokens saved \u{b7} 75% of reads through sieve \u{b7} ~$0.06\n\
             last query ask bigwidget"
        );
        assert!(session.to_pretty().starts_with("{\n  \"id\": \"x\",\n  \"lastQuery\": \"ask bigwidget\",\n  \"perAgentQuery\": {},\n  \"toolReads\": 3,"));
    }

    #[test]
    fn test_p1_64_unparseable_file_reads_as_the_empty_session() {
        let s = Scratch::new("bad");
        s.write("x.json", "{not json");
        let session = latest_session(&s.0).expect("session");
        assert_eq!(
            session.to_pretty(),
            "{\n  \"id\": \"x\",\n  \"lastQuery\": null,\n  \"perAgentQuery\": {},\n  \"toolReads\": 0,\n  \"sourceReads\": 0,\n  \"savedTokens\": 0,\n  \"injectedPointers\": [],\n  \"nudges\": 0\n}"
        );
        assert_eq!(
            format_stats(&Ui::plain(), Some(&session), 0, 0),
            "no saved tokens yet \u{2014} they appear after an agent session uses sieve"
        );
        s.write("x.json", "{}");
        assert_eq!(
            latest_session(&s.0).expect("session").to_pretty(),
            "{\n  \"id\": \"x\"\n}"
        );
    }

    #[test]
    fn short_counts_use_k_and_m() {
        assert_eq!(short_count(950), "950");
        assert_eq!(short_count(1_500), "1.5k");
        assert_eq!(short_count(123_456), "123k");
        assert_eq!(short_count(1_234_567), "1.2M");
    }

    /// A fixture store: two sessions, three days. Pins the report fields.
    #[test]
    fn test_savings_report_gives_session_today_and_seven_days() {
        let s = Scratch::new("report");
        // 2026-10-04 12:00 UTC.
        let now = 1_791_115_200u64;
        s.write(
            "a.json",
            r#"{"savedTokens":3000,"savedByDay":{"2026-10-04":1000,"2026-10-02":2000},"inputCostMicros":5000000,"inputTokensBilled":1000000}"#,
        );
        s.write(
            "b.json",
            r#"{"savedTokens":500,"savedByDay":{"2026-10-04":500}}"#,
        );
        s.write(
            "c.json",
            r#"{"savedTokens":9,"savedByDay":{"2026-09-01":9}}"#,
        );
        let latest = latest_session(&s.0);
        let report = Json::Obj(savings_report(&s.0.join("sieve"), latest.as_ref(), now, 0));
        let today = report.get("today").expect("today");
        assert_eq!(today.get("date").and_then(Json::as_str), Some("2026-10-04"));
        assert_eq!(today.get("tokens").and_then(Json::as_f64), Some(1500.0));
        assert_eq!(today.get("dollars").and_then(Json::as_f64), Some(0.005));
        let Some(Json::Arr(days)) = report.get("last7days") else {
            panic!("last7days is not a list");
        };
        assert_eq!(days.len(), 7);
        let tokens: Vec<f64> = days
            .iter()
            .map(|d| d.get("tokens").and_then(Json::as_f64).unwrap_or(-1.0))
            .collect();
        assert_eq!(tokens, vec![1500.0, 0.0, 2000.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(report
            .get("session")
            .and_then(|s| s.get("tokens"))
            .is_some());
        assert!(report
            .get("basis")
            .and_then(Json::as_str)
            .is_some_and(|b| b.contains("input")));
    }

    #[test]
    fn test_p1_64_sub_cent_value_and_locale_grouping() {
        assert_eq!(format_dollars(0.001), "<$0.01");
        assert_eq!(format_dollars(12.345), "$12.35");
        assert_eq!(dollars_saved(100.0, 0.0, 1.0), None);
        assert_eq!(dollars_saved(0.0, 5.0, 1.0), None);
        assert_eq!(to_locale_string(1234567.0), "1,234,567");
        assert_eq!(to_locale_string(999.0), "999");
        assert_eq!(to_locale_string(1000.5), "1,000.5");
    }
}
