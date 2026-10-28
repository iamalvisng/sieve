//! The `sieve rename` subcommand: a cross-file rename of one symbol from
//! proven edit sites (design `docs/design/rename.md`, section C). The
//! default is a dry run. `--write` plans again under the build lock and
//! writes only a plan that `--expect` names. Exit 0 is a ready or written
//! plan, exit 2 is a refusal, exit 1 is a usage or I/O error.

use std::fs;
use std::io::Write;
use std::path::Path;

use clap::Args;
use sieve_parse::rename::{
    apply_plan, discard_journal, plan_rename, recover, render_diff, ApplyReport, ApplyStatus, Plan,
    Reason, RenameError, Status,
};

use crate::query;

/// The most "not edited" rows that the text output lists.
const REPORT_ROWS: usize = 20;
/// The journal path under the context dir, as sieve-parse writes it.
const JOURNAL_FILE: &str = ".cache/rename-journal.json";
/// The most characters of source text in one row.
const TEXT_CHARS: usize = 100;

/// Flags for `sieve rename`.
#[derive(Args, Debug)]
pub struct RenameArgs {
    /// The symbol: a bare name, a node id `path#name`, or `path:line`.
    pub symbol: Option<String>,

    /// The new name.
    pub new_name: Option<String>,

    /// Writes the files. Without it, the command is a dry run.
    #[arg(long)]
    pub write: bool,

    /// With `--write`, writes only when the new plan has this id.
    #[arg(long, value_name = "plan-id")]
    pub expect: Option<String>,

    /// Prints the plan or the result as JSON.
    #[arg(long)]
    pub json: bool,

    /// The number of context lines in the diff.
    #[arg(long, value_name = "n", default_value_t = 0)]
    pub context: usize,

    /// Narrows the symbol search to this path. It never narrows the scan.
    #[arg(long = "in", value_name = "path")]
    pub in_path: Option<String>,

    /// Restores the files from a journal that a crash left.
    #[arg(long, conflicts_with_all = ["write", "discard_journal"])]
    pub recover: bool,

    /// Removes a left journal without restoring. Use it after `--recover`
    /// reports conflicts and you checked those files.
    #[arg(long = "discard-journal", conflicts_with = "write")]
    pub discard_journal: bool,
}

/// Runs `sieve rename`. A refusal exits 2 after it prints its report.
pub fn run(args: &RenameArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = query::query_root(None, dir_override, &cwd);
    let context_dir = query::context_dir(&root, dir_override, &cwd);
    if let Some(dir) = dir_override {
        if !context_dir.is_dir() {
            return Err(format!(
                "directory not found: {} \u{2014} check the --dir path",
                dir.display()
            ));
        }
    }
    if args.discard_journal {
        if std::fs::symlink_metadata(context_dir.join(JOURNAL_FILE)).is_err() {
            emit("no journal to discard.\n");
            return Ok(());
        }
        discard_journal(&root, &context_dir).map_err(error_text)?;
        emit("journal discarded. Graph refreshed.\n");
        return Ok(());
    }
    if args.recover {
        return run_recover(args, &root, &context_dir);
    }
    let (symbol, new_name) = match (args.symbol.as_deref(), args.new_name.as_deref()) {
        (Some(symbol), Some(new_name)) => (symbol, new_name),
        _ => return Err("missing argument: give <symbol> <new-name>".to_string()),
    };
    if args.write {
        return run_write(args, &root, &context_dir, symbol, new_name);
    }
    if args.expect.is_some() {
        return Err("--expect needs --write".to_string());
    }
    let plan = plan_rename(
        &root,
        &context_dir,
        symbol,
        new_name,
        args.in_path.as_deref(),
    )
    .map_err(error_text)?;
    if args.json {
        emit(&to_json(&plan)?);
    } else if plan.status == Status::Ready {
        let write_cmd = write_command(args, symbol, new_name, &plan.plan_id, dir_override);
        let diff = render_diff(&plan, &root, args.context);
        emit(&dry_run_text(&plan, &root, &write_cmd, &clean(&diff)));
    } else {
        emit(&refusal_text(&plan));
    }
    finish(plan.status == Status::Ready)
}

/// Runs `--write`: plans again under the lock and writes.
fn run_write(
    args: &RenameArgs,
    root: &Path,
    context_dir: &Path,
    symbol: &str,
    new_name: &str,
) -> Result<(), String> {
    let report = apply_plan(
        root,
        context_dir,
        symbol,
        new_name,
        args.in_path.as_deref(),
        args.expect.as_deref(),
    )
    .map_err(error_text)?;
    if args.json {
        emit(&to_json(&report)?);
    } else {
        emit(&write_text(&report, args.expect.as_deref()));
    }
    finish(report.status == ApplyStatus::Written)
}

/// Runs `--recover`: restores the files from a left journal.
fn run_recover(args: &RenameArgs, root: &Path, context_dir: &Path) -> Result<(), String> {
    let report = recover(root, context_dir).map_err(error_text)?;
    if args.json {
        emit(&to_json(&report)?);
    } else if !report.found {
        emit("no rename journal found. Nothing to recover.\n");
    } else {
        let mut out = String::new();
        let count = report.restored.len();
        if count > 0 {
            out.push_str(&format!(
                "recovered {count} {}: {}.\n",
                plural(count, "file", "files"),
                report.restored.join(", ")
            ));
        }
        if report.conflicts.is_empty() {
            if count == 0 {
                out.push_str("journal removed. No file needed a rewrite.\n");
            } else {
                out.push_str("Journal removed. Graph refreshed.\n");
            }
        } else {
            out.push_str(&format!(
                "left alone, changed by someone else: {}\nThe journal is kept. Run `sieve rename --discard-journal` after you check these files.\n",
                report.conflicts.join(", ")
            ));
        }
        emit(&out);
    }
    finish(report.conflicts.is_empty())
}

/// The error line of a failed rename. A failed restore names the files and
/// the journal that Sieve kept.
fn error_text(err: RenameError) -> String {
    match err {
        RenameError::RestoreFailed {
            why,
            files,
            journal,
        } => format!(
            "rename failed: {why}. Sieve could not restore {}. The journal {} is kept. Run `sieve rename --recover` after you check these files.",
            files.join(", "),
            journal.display()
        ),
        other => other.to_string(),
    }
}

/// Removes terminal control characters from text that comes from a repo:
/// ESC sequences, C0 and C1 controls and DEL. Tab and newline stay.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek().copied() {
                // CSI: parameters, then one final byte 0x40-0x7e.
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC backslash.
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\t' | '\n' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Prints `text` on stdout and flushes it, so an exit cannot lose it.
fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// Ends the command: success gives `Ok`, a refusal exits 2.
fn finish(ok: bool) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        std::process::exit(2);
    }
}

/// Renders a value as pretty JSON with a closing newline.
fn to_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// Quotes one shell word when it holds a character that a shell reads.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:#@=+,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The exact command that writes this plan.
fn write_command(
    args: &RenameArgs,
    symbol: &str,
    new_name: &str,
    plan_id: &str,
    dir_override: Option<&Path>,
) -> String {
    let mut cmd = String::from("sieve");
    if let Some(dir) = dir_override {
        cmd.push_str(&format!(" --dir {}", shell_word(&dir.to_string_lossy())));
    }
    cmd.push_str(&format!(
        " rename {} {}",
        shell_word(symbol),
        shell_word(new_name)
    ));
    if let Some(path) = &args.in_path {
        cmd.push_str(&format!(" --in {}", shell_word(path)));
    }
    cmd.push_str(&format!(" --write --expect {plan_id}"));
    cmd
}

fn edit_count(plan: &Plan) -> usize {
    plan.files.iter().map(|f| f.edits.len()).sum()
}

fn header_tail(plan: &Plan) -> String {
    format!("{} ({}) -> {}", plan.target.id, plan.target.kind, plan.new)
}

/// The dry-run text: header, the write command, the diff and the report.
fn dry_run_text(plan: &Plan, root: &Path, write_cmd: &str, diff: &str) -> String {
    let files = plan.files.len();
    let edits = edit_count(plan);
    let mut out = format!(
        "rename plan {}: {}\n{files} {}, {edits} {}. Dry run: no file changed.\nWrite it: {write_cmd}\n",
        plan.plan_id,
        header_tail(plan),
        plural(files, "file", "files"),
        plural(edits, "edit", "edits"),
    );
    if !diff.is_empty() {
        out.push('\n');
        out.push_str(diff);
    }
    out.push_str(&report_rows(plan, root));
    sieve_savings::append_line(&out, &sieve_savings::rename_line(files))
}

/// One source line, trimmed and cut to a short length.
fn line_text(root: &Path, path: &str, line: usize) -> String {
    let Ok(text) = fs::read_to_string(root.join(path)) else {
        return String::new();
    };
    let row = text.lines().nth(line.saturating_sub(1)).unwrap_or("");
    clean(row).trim().chars().take(TEXT_CHARS).collect()
}

/// The "not edited" rows: hits that the plan lists and never edits.
fn report_rows(plan: &Plan, root: &Path) -> String {
    let rows: Vec<(String, &str, String)> = plan
        .report
        .iter()
        .take(REPORT_ROWS)
        .map(|r| {
            (
                format!("{}:{}", r.path, r.line),
                r.class.as_str(),
                line_text(root, &r.path, r.line),
            )
        })
        .collect();
    let loc_w = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0);
    let class_w = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0);
    let mut out = String::new();
    for (loc, class, text) in &rows {
        let row = format!("not edited  {loc:<loc_w$}  {class:<class_w$}  {text}");
        out.push_str(clean(&row).trim_end());
        out.push('\n');
    }
    if plan.report.len() > REPORT_ROWS {
        out.push_str(&format!(
            "not edited  ... and {} more (see --json)\n",
            plan.report.len() - REPORT_ROWS
        ));
    }
    out
}

/// The refusal text: header, count line and one row per cause.
fn refusal_text(plan: &Plan) -> String {
    let n = plan.refused.len();
    let mut out = format!(
        "rename refused: {}\nSieve cannot prove {n} {}. No file changed.\n",
        header_tail(plan),
        plural(n, "site", "sites"),
    );
    let loc_w = plan
        .refused
        .iter()
        .map(|r| format!("{}:{}:{}", r.path, r.line, r.col).chars().count())
        .max()
        .unwrap_or(0);
    let reason_w = plan
        .refused
        .iter()
        .map(|r| reason_name(r.reason).len())
        .max()
        .unwrap_or(0);
    for r in &plan.refused {
        let loc = format!("{}:{}:{}", r.path, r.line, r.col);
        let row = format!(
            "  {loc:<loc_w$}  {:<reason_w$}  {}",
            reason_name(r.reason),
            r.text
        );
        out.push_str(clean(&row).trim_end());
        out.push('\n');
    }
    if plan
        .refused
        .iter()
        .any(|r| r.reason == Reason::JournalPresent)
    {
        out.push_str("Run sieve rename --recover to restore the files.\n");
    }
    out
}

/// The snake_case name of a refusal reason, as the JSON shows it.
fn reason_name(reason: Reason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The text of a `--write` result.
fn write_text(report: &ApplyReport, expect: Option<&str>) -> String {
    let plan = &report.plan;
    match report.status {
        ApplyStatus::Written => {
            let files = plan.files.len();
            let edits = edit_count(plan);
            format!(
                "renamed {} -> {}: {files} {}, {edits} {}. Graph refreshed. Check passed.\n",
                plan.target.id,
                plan.new,
                plural(files, "file", "files"),
                plural(edits, "edit", "edits"),
            )
        }
        ApplyStatus::Refused => {
            if !plan.refused.is_empty() {
                return refusal_text(plan);
            }
            let mut out = format!("rename refused: {}\n", header_tail(plan));
            if let Some(message) = &report.message {
                out.push_str(&format!("{}\n", clean(message)));
            }
            out
        }
        ApplyStatus::ExpectMismatch => {
            let message = report.message.clone().unwrap_or_else(|| {
                format!(
                    "plan id is now {}, not {}",
                    plan.plan_id,
                    expect.unwrap_or("the expected id")
                )
            });
            format!("rename not written: {message}. No file changed.\n")
        }
        ApplyStatus::RollbackConflict | ApplyStatus::RolledBack => {
            let mut out = match &report.message {
                Some(message) => format!("{}\n", clean(message)),
                None => "rolled back: the post-check failed\n".to_string(),
            };
            if !report.files.is_empty() {
                out.push_str(&format!("Restored: {}\n", clean(&report.files.join(", "))));
            }
            if !report.conflicts.is_empty() {
                out.push_str(&format!(
                    "Left alone, changed by someone else: {}\nRun `sieve rename --recover`, then `sieve rename --discard-journal` after you check these files.\n",
                    clean(&report.conflicts.join(", "))
                ));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_parse::rename::{Edit, PlanFile, Refusal, SiteClass, Target};

    #[test]
    fn shell_word_quotes_only_when_needed() {
        assert_eq!(shell_word("parseArgs"), "parseArgs");
        assert_eq!(shell_word("src/a.ts#f"), "src/a.ts#f");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
    }

    #[test]
    fn reason_name_is_snake_case() {
        assert_eq!(reason_name(Reason::DynamicImport), "dynamic_import");
        assert_eq!(reason_name(Reason::JournalPresent), "journal_present");
    }

    fn sample_plan(refused: Vec<Refusal>) -> Plan {
        Plan {
            version: 1,
            plan_id: "3f9a1c2e".to_string(),
            status: if refused.is_empty() {
                Status::Ready
            } else {
                Status::Refused
            },
            target: Target {
                id: "src/a.ts#f".to_string(),
                kind: "function".to_string(),
                path: "src/a.ts".to_string(),
                line: 1,
            },
            old: "f".to_string(),
            new: "g".to_string(),
            files: vec![PlanFile {
                path: "src/a.ts".to_string(),
                sha256: "00".to_string(),
                edits: vec![Edit {
                    start: 0,
                    end: 1,
                    line: 1,
                    col: 1,
                    site: SiteClass::Declaration,
                }],
            }],
            refused,
            report: Vec::new(),
        }
    }

    fn sample_report(status: ApplyStatus, message: Option<&str>) -> ApplyReport {
        ApplyReport {
            status,
            plan: sample_plan(Vec::new()),
            files: Vec::new(),
            message: message.map(str::to_string),
            conflicts: Vec::new(),
        }
    }

    #[test]
    fn write_text_written() {
        let r = sample_report(ApplyStatus::Written, None);
        assert_eq!(
            write_text(&r, None),
            "renamed src/a.ts#f -> g: 1 file, 1 edit. Graph refreshed. Check passed.\n"
        );
    }

    #[test]
    fn write_text_refused_without_rows_shows_message() {
        let r = sample_report(
            ApplyStatus::Refused,
            Some("a file failed the pre-write checks"),
        );
        assert_eq!(
            write_text(&r, None),
            "rename refused: src/a.ts#f (function) -> g\na file failed the pre-write checks\n"
        );
    }

    #[test]
    fn write_text_refused_with_rows_shows_rows_once() {
        let mut r = sample_report(
            ApplyStatus::Refused,
            Some("the plan is refused: 1 cause(s)"),
        );
        r.plan = sample_plan(vec![Refusal {
            path: "src/a.ts".to_string(),
            line: 3,
            col: 4,
            reason: Reason::Collision,
            text: "g is declared".to_string(),
        }]);
        let text = write_text(&r, None);
        assert!(text.starts_with("rename refused: "), "{text}");
        assert!(
            text.contains("src/a.ts:3:4  collision  g is declared"),
            "{text}"
        );
        assert!(!text.contains("cause(s)"), "{text}");
    }

    #[test]
    fn write_text_expect_mismatch() {
        let r = sample_report(
            ApplyStatus::ExpectMismatch,
            Some("plan id 3f9a1c2e does not match expected 0000"),
        );
        assert_eq!(
            write_text(&r, Some("0000")),
            "rename not written: plan id 3f9a1c2e does not match expected 0000. No file changed.\n"
        );
    }

    #[test]
    fn write_text_rolled_back_prints_once() {
        let mut r = sample_report(
            ApplyStatus::RolledBack,
            Some("rolled back: the check failed"),
        );
        assert_eq!(write_text(&r, None), "rolled back: the check failed\n");
        r.files = vec!["src/a.ts".to_string()];
        assert_eq!(
            write_text(&r, None),
            "rolled back: the check failed\nRestored: src/a.ts\n"
        );
    }

    #[test]
    fn write_text_rollback_conflict_lists_conflicts_once() {
        let mut r = sample_report(
            ApplyStatus::RollbackConflict,
            Some("rolled back, but b.ts changed"),
        );
        r.files = vec!["a.ts".to_string()];
        r.conflicts = vec!["b.ts".to_string()];
        let text = write_text(&r, None);
        assert_eq!(text.matches("rolled back").count(), 1, "{text}");
        assert!(text.contains("Restored: a.ts\n"), "{text}");
        assert!(text.contains("changed by someone else: b.ts\n"), "{text}");
        assert_eq!(text.matches("--discard-journal").count(), 1, "{text}");
    }

    #[test]
    fn clean_strips_escapes_and_controls() {
        assert_eq!(clean("a\u{1b}[31mred\u{1b}[0m b"), "ared b");
        assert_eq!(clean("x\u{1b}]0;title\u{7}y"), "xy");
        assert_eq!(clean("x\u{1b}]0;t\u{1b}\\y"), "xy");
        assert_eq!(clean("a\u{0}b\u{8}c\u{7f}d\re"), "abcde");
        assert_eq!(clean("a\tb\nc"), "a\tb\nc");
        assert_eq!(clean("caf\u{e9} \u{9b}31m"), "caf\u{e9} 31m");
    }

    #[test]
    fn plural_picks_by_count() {
        assert_eq!(plural(1, "file", "files"), "file");
        assert_eq!(plural(0, "file", "files"), "files");
    }
}
