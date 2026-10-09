//! Phase 2 feature F1, unit 1: classify a hook call as a file read or a
//! call to leave alone. The module is pure. It reads a file only to
//! measure the bytes the tool call would return. A `grep`, an `rg` and a
//! Grep tool call pass through, because F1 dropped its Grep path.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sieve_savings::to_tokens;

/// The most lines one Read call returns. The number comes from the Read
/// tool, not from Sieve. If the Read tool changes its cap, edit this line.
const READ_LINE_CAP: u64 = 2000;

/// The most tokens one Read call returns. The number comes from a measured
/// probe of Claude Code on 2026-09-30, not from Sieve. Read returned 386
/// lines of a 1,111-line file and named this cap in its notice. Sieve
/// counts the tokens with `to_tokens`, the estimate every savings line uses.
const READ_TOKEN_CAP: u64 = 25_000;

/// The most bytes Claude Code passes whole from one Bash output. The number
/// comes from a measured probe of Claude Code on 2026-09-30, not from Sieve.
/// A 29,999-byte `cat` arrived whole. A 30,001-byte `cat` arrived as a
/// preview.
pub(crate) const BASH_OUTPUT_CAP: u64 = 30_000;

/// The head preview Claude Code passes above `BASH_OUTPUT_CAP`. The number
/// comes from the same probe, not from Sieve. The preview held 33 full lines
/// of 59 bytes in every case. It cuts at a line boundary under 2,000 bytes.
const BASH_PREVIEW_BYTES: u64 = 1_947;

/// The notice Claude Code adds above `BASH_OUTPUT_CAP`. The number comes from
/// the same probe, not from Sieve. The measured text is `Output too large
/// (29.3KB). Full output saved to: <path>`, a blank line, then `Preview
/// (first 2KB):`. The path makes the length vary, so 300 is an estimate.
const BASH_NOTICE_BYTES: u64 = 300;

/// What the agent wants from one tool call.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    /// The agent wants the content of one file. `raw_bytes` is the count
    /// of bytes the tool call would return, not the file size. `bounded`
    /// is true when the call names its own bound: a line count, a byte
    /// count, a line span, or a Read `offset` or `limit`. A bounded call
    /// asks for a part of the file. F1 exists to stop a wholesale read, so
    /// F1 never rewrites a bounded call. A live probe on 2026-09-30 found
    /// the reason: F1 named `sed -n '6,200p'`, then rewrote that command,
    /// and the agent looped.
    ReadFile {
        path: String,
        raw_bytes: u64,
        bounded: bool,
    },
    /// F1 does nothing with this call.
    PassThrough,
}

/// Classifies the hook stdin JSON value into an `Intent`.
pub(crate) fn classify(input: &Value) -> Intent {
    let cwd = input.get("cwd").and_then(Value::as_str);
    let tool_input = &input["tool_input"];
    let intent = match input.get("tool_name").and_then(Value::as_str) {
        Some("Read") => classify_read(tool_input, cwd),
        Some("Bash") => tool_input
            .get("command")
            .and_then(Value::as_str)
            .and_then(|c| classify_bash(c, cwd))
            .map(cap_bash_output),
        _ => None,
    };
    intent.unwrap_or(Intent::PassThrough)
}

/// Caps the raw cost of a Bash read at what Claude Code passes the agent.
/// Above `BASH_OUTPUT_CAP`, the agent receives the preview plus the notice,
/// not the file.
fn cap_bash_output(intent: Intent) -> Intent {
    match intent {
        Intent::ReadFile {
            path,
            raw_bytes,
            bounded,
        } if raw_bytes > BASH_OUTPUT_CAP => Intent::ReadFile {
            path,
            raw_bytes: BASH_PREVIEW_BYTES + BASH_NOTICE_BYTES,
            bounded,
        },
        other => other,
    }
}

/// Resolves a path. A relative path resolves against the hook's `cwd`
/// field, not against the project dir. Without a `cwd`, it does not resolve.
fn resolve(path: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let p = Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd?).join(p)
    };
    // A directory or a missing file is not a read of one file.
    full.is_file().then_some(full)
}

/// Counts the bytes of `take` lines after skipping `skip` lines. The count
/// stops at the last whole line that keeps `to_tokens` at or under
/// `token_cap`. The Read tool cuts at a line boundary the same way.
fn line_bytes(data: &[u8], skip: u64, take: u64, token_cap: u64) -> u64 {
    let mut total = 0u64;
    for line in data
        .split_inclusive(|b| *b == b'\n')
        .skip(usize::try_from(skip).unwrap_or(usize::MAX))
        .take(usize::try_from(take).unwrap_or(usize::MAX))
    {
        let next = total + line.len() as u64;
        if to_tokens(next) > token_cap {
            break;
        }
        total = next;
    }
    total
}

fn read_intent(path: &str, cwd: Option<&str>, skip: u64, take: Option<u64>) -> Option<Intent> {
    let full = resolve(path, cwd)?;
    let raw_bytes = match (skip, take) {
        (0, None) => std::fs::metadata(&full).ok()?.len(),
        (_, t) => line_bytes(
            &std::fs::read(&full).ok()?,
            skip,
            t.unwrap_or(u64::MAX),
            u64::MAX,
        ),
    };
    Some(Intent::ReadFile {
        path: full.to_string_lossy().into_owned(),
        raw_bytes,
        // `cat`, `less` and `bat` read the whole file. Every other form
        // names a bound.
        bounded: skip != 0 || take.is_some(),
    })
}

// ponytail: the Read tool also truncates a very long line. This code does
// not model that. The cost stays a small overestimate for such a file.
fn classify_read(ti: &Value, cwd: Option<&str>) -> Option<Intent> {
    let path = ti.get("file_path")?.as_str()?;
    let offset = ti.get("offset").and_then(Value::as_u64).unwrap_or(0);
    // The Read tool counts lines from 1.
    let skip = offset.saturating_sub(1);
    // The Read tool returns at most `READ_LINE_CAP` lines.
    let take = ti
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(READ_LINE_CAP, |l| l.min(READ_LINE_CAP));
    let full = resolve(path, cwd)?;
    // The Read tool also returns at most `READ_TOKEN_CAP` tokens. The
    // smaller of the two caps sets the cost.
    let raw_bytes = line_bytes(&std::fs::read(&full).ok()?, skip, take, READ_TOKEN_CAP);
    Some(Intent::ReadFile {
        path: full.to_string_lossy().into_owned(),
        raw_bytes,
        bounded: ti.get("offset").is_some() || ti.get("limit").is_some(),
    })
}

/// True if the command is compound or holds text this code cannot parse.
pub(crate) fn is_unsafe_command(cmd: &str) -> bool {
    cmd.contains([
        '|', '>', '<', ';', '`', '&', '\n', '\r', '"', '\\', '$', '~', '*', '?', '[', '{',
    ])
}

/// Removes one pair of single quotes from a token with no space inside.
fn unquote(tok: &str) -> Option<&str> {
    let inner = match tok.strip_prefix('\'') {
        Some(rest) => rest.strip_suffix('\'')?,
        None => tok,
    };
    (!inner.contains('\'') && !inner.is_empty()).then_some(inner)
}

/// A path token: no flag, no quote.
fn path_token(tok: &str) -> Option<&str> {
    (!tok.starts_with('-') && !tok.contains('\'')).then_some(tok)
}

fn classify_bash(cmd: &str, cwd: Option<&str>) -> Option<Intent> {
    if is_unsafe_command(cmd) {
        return None;
    }
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    match toks.as_slice() {
        ["cat" | "less" | "bat", file] => read_intent(path_token(file)?, cwd, 0, None),
        ["head", file] => head_intent(10, file, cwd),
        ["tail", file] => tail_intent(10, file, cwd),
        ["head", flag, file] if count_flag(flag).is_some() => {
            head_intent(count_flag(flag)?, file, cwd)
        }
        ["tail", flag, file] if count_flag(flag).is_some() => {
            tail_intent(count_flag(flag)?, file, cwd)
        }
        ["head", "-n", n, file] => head_intent(n.parse().ok()?, file, cwd),
        ["tail", "-n", n, file] => tail_intent(n.parse().ok()?, file, cwd),
        ["head" | "tail", "-c", n, file] => bytes_intent(n.parse().ok()?, file, cwd),
        ["sed", "-n", script, file] => sed_intent(unquote(script)?, file, cwd),
        _ => None,
    }
}

/// Parses the one-token line count forms `-5` and `-n5`.
fn count_flag(tok: &str) -> Option<u64> {
    let digits = tok.strip_prefix("-n").or_else(|| tok.strip_prefix('-'))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn head_intent(n: u64, file: &str, cwd: Option<&str>) -> Option<Intent> {
    read_intent(path_token(file)?, cwd, 0, Some(n))
}

fn tail_intent(n: u64, file: &str, cwd: Option<&str>) -> Option<Intent> {
    let file = path_token(file)?;
    let data = std::fs::read(resolve(file, cwd)?).ok()?;
    let total = data.split_inclusive(|b| *b == b'\n').count() as u64;
    // Skip every line before the last `n`.
    read_intent(file, cwd, total.saturating_sub(n), Some(n))
}

/// The `-c` form: the cost is `n` bytes, capped at the file size.
fn bytes_intent(n: u64, file: &str, cwd: Option<&str>) -> Option<Intent> {
    let full = resolve(path_token(file)?, cwd)?;
    let len = std::fs::metadata(&full).ok()?.len();
    Some(Intent::ReadFile {
        path: full.to_string_lossy().into_owned(),
        raw_bytes: n.min(len),
        bounded: true,
    })
}

/// Handles the script `N,Mp` or `Np` of `sed -n`.
fn sed_intent(script: &str, file: &str, cwd: Option<&str>) -> Option<Intent> {
    let body = script.strip_suffix('p')?;
    let (a, b) = match body.split_once(',') {
        Some((a, b)) => (a.parse::<u64>().ok()?, b.parse::<u64>().ok()?),
        None => (body.parse::<u64>().ok()?, body.parse::<u64>().ok()?),
    };
    if a == 0 || b < a {
        return None;
    }
    read_intent(path_token(file)?, cwd, a - 1, Some(b - a + 1))
}

/// The lines a narrowed Read keeps. The 2026-09-29 probe used 5. The
/// `PreToolUse` hook must pass this same number in `updatedInput`, so the
/// note and the narrowing agree.
pub(crate) const NARROWED_LINES: u64 = 5;

/// Counts the lines of `data` the way the Read tool numbers them. A final
/// newline ends the last line. It does not start an empty one.
fn line_count(data: &[u8]) -> u64 {
    data.split_inclusive(|b| *b == b'\n').count() as u64
}

/// Answers an intent from the graph on disk. `None` means F1 has nothing
/// better than the raw call, so the caller passes the call through.
///
/// A `ReadFile` answer is the file skeleton. `answer` reads it from the
/// cached `<context_dir>/.graph/wiring.json`. `answer` never builds or
/// refreshes the graph. A missing graph, or a file the graph does not
/// hold, gives `None`. Every answer ends with one branded note line.
pub(crate) fn answer(root: &Path, context_dir: &Path, intent: &Intent) -> Option<String> {
    answer_via(root, context_dir, intent, false)
}

/// Like `answer`, for a Bash read. The note names `sed -n` first, because
/// a shell command has no `offset` and no `limit`. It names the Read tool
/// second, so the agent has two routes to the body.
pub(crate) fn answer_bash(root: &Path, context_dir: &Path, intent: &Intent) -> Option<String> {
    answer_via(root, context_dir, intent, true)
}

fn answer_via(root: &Path, context_dir: &Path, intent: &Intent, bash: bool) -> Option<String> {
    let Intent::ReadFile { path, .. } = intent else {
        // The full load still runs first, as before: it records a capped wiring.
        crate::hook::read_wiring(context_dir)?;
        return None;
    };
    // The order is deliberate: an out-of-root path returns before any wiring read.
    let rel = rel_to_root(root, path)?;
    let result = match crate::hook::open_lookup(context_dir) {
        Some(mut lookup) => skeleton_from_lookup(lookup.record(&rel), &rel),
        None => {
            let graph = crate::hook::read_wiring(context_dir)?;
            sieve_query::skeleton::skeleton(Some(&graph), &rel)
        }
    };
    answer_read(&result, path, &rel, bash)
}

/// The skeleton of `rel` from its lookup record. A missing record gives no
/// entries, as the full path gives for an unknown file.
fn skeleton_from_lookup(
    record: Option<sieve_core::lookup::Record>,
    rel: &str,
) -> sieve_query::skeleton::SkeletonResult {
    let entries = record
        .into_iter()
        .flat_map(|r| r.nodes)
        .filter(|n| n.kind != sieve_core::wiring::Kind::File)
        .map(|n| sieve_query::skeleton::SkeletonEntry {
            name: n.name,
            kind: n.kind,
            span: n.span,
            signature: n.signature,
            summary: n.summary,
            callers: n.callers,
        })
        .collect();
    sieve_query::skeleton::SkeletonResult {
        file: rel.to_string(),
        entries,
        note: None,
    }
}

/// Turns an absolute or relative path into the graph's root-relative form.
fn rel_to_root(root: &Path, path: &str) -> Option<String> {
    let p = Path::new(path);
    let rel = if p.is_absolute() {
        p.strip_prefix(root).ok()?
    } else {
        p
    };
    Some(rel.to_string_lossy().into_owned())
}

fn answer_read(
    result: &sieve_query::skeleton::SkeletonResult,
    path: &str,
    rel: &str,
    bash: bool,
) -> Option<String> {
    // `skeleton` also matches a path suffix. Only the exact path counts.
    if result.entries.is_empty() || result.file != rel {
        return None;
    }
    let total = line_count(&std::fs::read(path).ok()?);
    let shown = NARROWED_LINES.min(total);
    let note = if bash {
        &format!(
            "[sieve] F1: a hook replaced your command. You got {shown} of {total} lines. \
             For more, run `sed -n '{},200p' {path}`, or Read with offset and limit.",
            shown + 1
        )
    } else {
        &format!(
            "[sieve] F1 narrowed this Read to {shown} of {total} lines of {rel}. Read a span \
             with offset and limit, or run `sieve ask \"<name>\" --source` for a symbol."
        )
    };
    Some(format!(
        "{}{note}\n",
        sieve_query::skeleton::format_skeleton(result)
    ))
}

/// The smallest graph answer that counts as a real answer, in bytes. A Fable
/// 5.1 review on 2026-09-29 found the reason. `sieve skeleton` on a
/// non-source file prints "no definitions indexed", about 40 bytes. A plain
/// "smaller wins" rule lets that non-answer beat every JSON, Markdown, TOML
/// and lock file. Every real answer holds a note line, which is longer.
pub(crate) const MIN_ANSWER_BYTES: u64 = 64;

/// The graph answer must be at most `1 / MIN_SAVING_FACTOR` of the raw
/// cost. Interception costs the agent one turn to read the note and choose
/// its next step. A saving of a few bytes does not pay for that turn.
// ponytail: 2 is a choice, not a measurement. The F1 replay harness is
// what would set it. Tune it when that harness exists.
pub(crate) const MIN_SAVING_FACTOR: u64 = 2;

/// The smallest raw cost F1 intercepts, in bytes. Below it, `decide` passes
/// the call through no matter how good the ratio is. A live probe on
/// 2026-09-30 found the reason. F1 narrowed a 246-line file of 8,180 bytes
/// to five lines, because its 2,020-byte skeleton passed the ratio. The
/// agent then spent a second call to hold any of the body. For a file that
/// small, the whole file is the better answer.
// ponytail: 16,384 is a choice, not a measurement. The F1 replay harness
// is what would set it. It is twice the 8,180-byte loss and one third of
// the 49 KB file where the 2026-09-29 probe measured a win.
pub(crate) const MIN_RAW_BYTES: u64 = 16_384;

/// Decides whether F1 intercepts. It returns the graph answer to send, or
/// `None` to pass the call through. `raw_bytes` is the measured cost of the
/// raw call. `None` means nobody measured it.
///
/// F1 intercepts only when the answer is real, the raw cost is measured
/// and at least `MIN_RAW_BYTES`, the saving passes the ratio, and the
/// answer is smaller than the raw cost. `PassThrough` never intercepts.
pub(crate) fn decide(
    intent: &Intent,
    raw_bytes: Option<u64>,
    graph_answer: Option<String>,
) -> Option<String> {
    if *intent == Intent::PassThrough {
        return None;
    }
    let text = graph_answer?;
    let raw = raw_bytes?;
    if raw < MIN_RAW_BYTES {
        return None;
    }
    let len = text.len() as u64;
    if len < MIN_ANSWER_BYTES {
        return None;
    }
    if len.saturating_mul(MIN_SAVING_FACTOR) > raw {
        return None;
    }
    // Never cost more. This guard holds even if the ratio constant changes.
    (len < raw).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch dir that removes itself on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!(
                "sieve-intercept-{}-{n}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&p).expect("create temp dir");
            TempDir(p)
        }
        fn s(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A dir with `f.txt`: 100 lines, each "line NNN\n" (9 bytes).
    fn fixture() -> TempDir {
        let d = TempDir::new();
        let body: String = (1..=100).map(|i| format!("line {i:03}\n")).collect();
        std::fs::write(d.0.join("f.txt"), body).expect("write");
        std::fs::write(d.0.join("my file.txt"), "x\n").expect("write");
        std::fs::create_dir(d.0.join("sub")).expect("mkdir");
        d
    }

    fn bash(d: &TempDir, cmd: &str) -> Intent {
        classify(&json!({"tool_name":"Bash","cwd":d.s(),"tool_input":{"command":cmd}}))
    }

    /// A wholesale read of `f.txt` that costs `n` bytes.
    fn read_of(d: &TempDir, n: u64) -> Intent {
        size_of(d, "f.txt", n)
    }

    /// A bounded read of `f.txt` that costs `n` bytes.
    fn span_of(d: &TempDir, n: u64) -> Intent {
        span_size_of(d, "f.txt", n)
    }

    #[test]
    fn test_f1_classify_read_whole_file_costs_file_size() {
        let d = fixture();
        let i = classify(&json!({"tool_name":"Read","tool_input":{"file_path":d.0.join("f.txt")}}));
        assert_eq!(i, read_of(&d, 900));
    }

    #[test]
    fn test_f1_classify_read_limit_costs_only_those_lines() {
        let d = fixture();
        let f = d.0.join("f.txt");
        let i = classify(&json!({"tool_name":"Read","tool_input":{"file_path":f,"limit":5}}));
        assert_eq!(i, span_of(&d, 45));
        let i = classify(
            &json!({"tool_name":"Read","tool_input":{"file_path":f,"offset":10,"limit":2}}),
        );
        assert_eq!(i, span_of(&d, 18));
    }

    /// A dir with `big.txt`: 3000 lines, each "line NNNN\n" (10 bytes).
    fn big_fixture() -> TempDir {
        let d = TempDir::new();
        let body: String = (1..=3000).map(|i| format!("line {i:04}\n")).collect();
        std::fs::write(d.0.join("big.txt"), body).expect("write");
        std::fs::write(d.0.join("mid.txt"), body_of(1500)).expect("write");
        d
    }

    fn body_of(n: u32) -> String {
        (1..=n).map(|i| format!("line {i:04}\n")).collect()
    }

    fn read_call(d: &TempDir, name: &str, extra: Value) -> Intent {
        let mut ti = json!({"file_path": d.0.join(name)});
        if let (Some(t), Some(e)) = (ti.as_object_mut(), extra.as_object()) {
            t.extend(e.clone());
        }
        classify(&json!({"tool_name":"Read","tool_input":ti}))
    }

    /// A wholesale read of `name` that costs `n` bytes.
    fn size_of(d: &TempDir, name: &str, n: u64) -> Intent {
        Intent::ReadFile {
            path: d.0.join(name).to_string_lossy().into_owned(),
            raw_bytes: n,
            bounded: false,
        }
    }

    /// A bounded read of `name` that costs `n` bytes.
    fn span_size_of(d: &TempDir, name: &str, n: u64) -> Intent {
        Intent::ReadFile {
            path: d.0.join(name).to_string_lossy().into_owned(),
            raw_bytes: n,
            bounded: true,
        }
    }

    #[test]
    fn test_f1_classify_read_under_cap_costs_whole_file() {
        let d = big_fixture();
        assert_eq!(
            read_call(&d, "mid.txt", json!({})),
            size_of(&d, "mid.txt", 15_000)
        );
    }

    #[test]
    fn test_f1_classify_read_over_cap_costs_first_2000_lines() {
        let d = big_fixture();
        assert_eq!(
            read_call(&d, "big.txt", json!({})),
            size_of(&d, "big.txt", 20_000)
        );
        // With an offset, the count starts at that line.
        assert_eq!(
            read_call(&d, "big.txt", json!({"offset": 2501})),
            span_size_of(&d, "big.txt", 5_000)
        );
    }

    #[test]
    fn test_f1_classify_read_limit_above_cap_is_capped() {
        let d = big_fixture();
        assert_eq!(
            read_call(&d, "big.txt", json!({"limit": 2500})),
            span_size_of(&d, "big.txt", 20_000)
        );
        assert_eq!(
            read_call(&d, "big.txt", json!({"limit": 3})),
            span_size_of(&d, "big.txt", 30)
        );
    }

    /// Writes `name` as `lines` lines of `width` bytes each, newline included.
    fn put_lines(d: &TempDir, name: &str, lines: usize, width: usize) {
        let line = format!("{}\n", "x".repeat(width - 1));
        std::fs::write(d.0.join(name), line.repeat(lines)).expect("write");
    }

    #[test]
    fn test_f1_classify_read_long_lines_cost_the_token_cap() {
        let d = TempDir::new();
        // 2,000 lines of 100 bytes is 200,000 bytes, about 50,000 tokens.
        // The token cap keeps 1,000 whole lines: 100,000 bytes is 25,000
        // tokens, and the next line would pass the cap.
        put_lines(&d, "long.txt", 2_000, 100);
        assert_eq!(to_tokens(100_000), READ_TOKEN_CAP);
        assert_eq!(
            read_call(&d, "long.txt", json!({})),
            size_of(&d, "long.txt", 100_000)
        );
    }

    #[test]
    fn test_f1_classify_read_short_lines_cost_the_line_cap() {
        let d = TempDir::new();
        // 2,500 lines of 40 bytes. The line cap keeps 2,000 lines, which is
        // 80,000 bytes, about 20,000 tokens, under the token cap.
        put_lines(&d, "short.txt", 2_500, 40);
        assert!(to_tokens(80_000) < READ_TOKEN_CAP);
        assert_eq!(
            read_call(&d, "short.txt", json!({})),
            size_of(&d, "short.txt", 80_000)
        );
    }

    #[test]
    fn test_f1_classify_bash_cat_under_and_over_the_harness_cap() {
        let d = TempDir::new();
        // 29,999 bytes arrives whole. 30,001 bytes arrives as the preview
        // plus the notice, about 2,250 bytes, and not the file size.
        std::fs::write(d.0.join("under.txt"), "x".repeat(29_999)).expect("write");
        std::fs::write(d.0.join("over.txt"), "x".repeat(30_001)).expect("write");
        assert_eq!(bash(&d, "cat under.txt"), size_of(&d, "under.txt", 29_999));
        let over = bash(&d, "cat over.txt");
        assert_eq!(
            over,
            size_of(&d, "over.txt", BASH_PREVIEW_BYTES + BASH_NOTICE_BYTES)
        );
        let Intent::ReadFile { raw_bytes, .. } = over else {
            panic!("a cat is a read");
        };
        assert!((2_200..=2_300).contains(&raw_bytes), "{raw_bytes}");
        // The `-c` form and `head` cap the same way.
        assert_eq!(
            bash(&d, "head -c 30001 over.txt"),
            span_size_of(&d, "over.txt", BASH_PREVIEW_BYTES + BASH_NOTICE_BYTES)
        );
    }

    #[test]
    fn test_f1_classify_bash_head_tail_short_count_forms() {
        let d = fixture();
        for c in ["head -3 f.txt", "head -n3 f.txt", "head -n 3 f.txt"] {
            assert_eq!(bash(&d, c), span_of(&d, 27), "{c}");
        }
        for c in ["tail -4 f.txt", "tail -n4 f.txt", "tail -n 4 f.txt"] {
            assert_eq!(bash(&d, c), span_of(&d, 36), "{c}");
        }
        assert_eq!(bash(&d, "head -n f.txt"), Intent::PassThrough);
        assert_eq!(bash(&d, "head -3x f.txt"), Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_read_relative_path_uses_hook_cwd() {
        let d = fixture();
        let i =
            classify(&json!({"tool_name":"Read","cwd":d.s(),"tool_input":{"file_path":"f.txt"}}));
        assert_eq!(i, read_of(&d, 900));
        let i = classify(&json!({"tool_name":"Read","tool_input":{"file_path":"f.txt"}}));
        assert_eq!(i, Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_read_directory_or_missing_passes_through() {
        let d = fixture();
        for p in ["sub", "nope.txt"] {
            let i = classify(&json!({"tool_name":"Read","cwd":d.s(),"tool_input":{"file_path":p}}));
            assert_eq!(i, Intent::PassThrough, "{p}");
        }
    }

    #[test]
    fn test_f1_classify_grep_passes_through() {
        let d = fixture();
        for c in ["grep foo", "grep -rn foo src", "rg 'foo' src"] {
            assert_eq!(bash(&d, c), Intent::PassThrough, "{c}");
        }
        let i = classify(&json!({"tool_name":"Grep","tool_input":{"pattern":"foo"}}));
        assert_eq!(i, Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_other_tool_passes_through() {
        assert_eq!(classify(&json!({"tool_name":"Edit"})), Intent::PassThrough);
        assert_eq!(classify(&json!({})), Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_bash_cat_less_bat_cost_whole_file() {
        let d = fixture();
        for v in ["cat", "less", "bat"] {
            assert_eq!(bash(&d, &format!("{v} f.txt")), read_of(&d, 900), "{v}");
        }
    }

    #[test]
    fn test_f1_classify_bash_head_tail_lines_cost_n_lines() {
        let d = fixture();
        assert_eq!(bash(&d, "head -n 3 f.txt"), span_of(&d, 27));
        assert_eq!(bash(&d, "tail -n 4 f.txt"), span_of(&d, 36));
        // A bare `head` reads 10 lines. That is a bound too.
        assert_eq!(bash(&d, "head f.txt"), span_of(&d, 90));
    }

    #[test]
    fn test_f1_classify_bash_head_tail_bytes_cost_n_bytes() {
        let d = fixture();
        assert_eq!(bash(&d, "head -c 100 f.txt"), span_of(&d, 100));
        assert_eq!(bash(&d, "tail -c 7 f.txt"), span_of(&d, 7));
        assert_eq!(bash(&d, "head -c 99999 f.txt"), span_of(&d, 900));
    }

    #[test]
    fn test_f1_classify_bash_sed_range_costs_41_lines() {
        let d = fixture();
        assert_eq!(bash(&d, "sed -n '10,50p' f.txt"), span_of(&d, 41 * 9));
        assert_eq!(bash(&d, "sed -n '5p' f.txt"), span_of(&d, 9));
    }

    #[test]
    fn test_f1_classify_bash_compound_command_passes_through() {
        let d = fixture();
        for c in [
            "cat f.txt | head",
            "cat f.txt > o",
            "cat < f.txt",
            "cat f.txt && ls",
            "cat f.txt || ls",
            "cat f.txt; ls",
            "cat `x`",
            "cat $(x)",
            "cat f.txt &",
            "cat f.txt\nls",
        ] {
            assert_eq!(bash(&d, c), Intent::PassThrough, "{c:?}");
        }
    }

    #[test]
    fn test_f1_classify_bash_quoted_path_with_space_passes_through() {
        let d = fixture();
        assert_eq!(bash(&d, "cat 'my file.txt'"), Intent::PassThrough);
        assert_eq!(bash(&d, "cat \"my file.txt\""), Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_bash_variable_or_tilde_path_passes_through() {
        let d = fixture();
        for c in ["cat $FILE", "cat ${DIR}/f.txt", "cat ~/f.txt"] {
            assert_eq!(bash(&d, c), Intent::PassThrough, "{c}");
        }
    }

    #[test]
    fn test_f1_classify_bash_unnamed_flag_passes_through() {
        let d = fixture();
        assert_eq!(bash(&d, "cat -n f.txt"), Intent::PassThrough);
        assert_eq!(bash(&d, "head -n 3 -q f.txt"), Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_bash_wrapped_verb_passes_through() {
        let d = fixture();
        for c in [
            "/bin/cat f.txt",
            "command cat f.txt",
            "env X=1 cat f.txt",
            "timeout 5 cat f.txt",
        ] {
            assert_eq!(bash(&d, c), Intent::PassThrough, "{c}");
        }
    }

    #[test]
    fn test_f1_classify_bash_sed_in_place_passes_through() {
        let d = fixture();
        for c in [
            "sed -ni '1p' f.txt",
            "sed -n -i '1p' f.txt",
            "sed -n '1p' -i f.txt",
            "sed 's/a/b/' f.txt",
        ] {
            assert_eq!(bash(&d, c), Intent::PassThrough, "{c}");
        }
    }

    #[test]
    fn test_f1_classify_bash_directory_or_missing_file_passes_through() {
        let d = fixture();
        assert_eq!(bash(&d, "cat sub"), Intent::PassThrough);
        assert_eq!(bash(&d, "cat nope.txt"), Intent::PassThrough);
        assert_eq!(bash(&d, "head -n 2 nope.txt"), Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_bash_extra_file_argument_passes_through() {
        let d = fixture();
        assert_eq!(bash(&d, "cat f.txt f.txt"), Intent::PassThrough);
        assert_eq!(bash(&d, "cat"), Intent::PassThrough);
    }

    #[test]
    fn test_f1_classify_needs_no_product_env() {
        // The module reads no product name, so `SIEVE_PRODUCT` does not matter.
        let d = fixture();
        assert_eq!(bash(&d, "cat f.txt"), read_of(&d, 900));
    }

    fn put(d: &TempDir, rel: &str, body: &str) {
        let p = d.0.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, body).expect("write");
    }

    /// A repo with `src/lib.rs` (two functions) and `Cargo.lock`, with a
    /// built `wiring.json` under `sieve/`. The build is a real parse.
    fn graph_fixture() -> TempDir {
        let d = TempDir::new();
        put(
            &d,
            "src/lib.rs",
            "pub fn alpha() -> u32 {\n    1\n}\n\npub fn beta() {}\n",
        );
        put(&d, "Cargo.lock", "[[package]]\nname = \"alpha\"\n");
        let ctx = d.0.join("sieve");
        let graph = sieve_parse::build_graph(&d.0, &ctx).expect("build graph");
        sieve_core::write_graph(&graph, &ctx.join(".graph/wiring.json")).expect("write");
        d
    }

    fn read_intent_for(d: &TempDir, rel: &str) -> Intent {
        Intent::ReadFile {
            path: d.0.join(rel).to_string_lossy().into_owned(),
            raw_bytes: 0,
            bounded: false,
        }
    }

    fn last_line(text: &str) -> &str {
        text.trim_end().lines().last().unwrap_or("")
    }

    #[test]
    fn test_f1_answer_read_cached_skeleton_holds_signatures_and_note() {
        let d = graph_fixture();
        let text = answer(&d.0, &d.0.join("sieve"), &read_intent_for(&d, "src/lib.rs"))
            .expect("skeleton is cached");
        assert!(text.starts_with("src/lib.rs \u{b7} 2 symbols\n"), "{text}");
        assert!(text.contains("alpha  fn  () -> u32"), "{text}");
        assert!(text.contains("beta  fn"), "{text}");
        assert_eq!(
            last_line(&text),
            "[sieve] F1 narrowed this Read to 5 of 5 lines of src/lib.rs. Read a span with \
             offset and limit, or run `sieve ask \"<name>\" --source` for a symbol."
        );
    }

    #[test]
    fn test_f1_answer_read_without_cached_graph_is_none() {
        let d = graph_fixture();
        let ctx = d.0.join("sieve");
        std::fs::remove_file(ctx.join(".graph/wiring.json")).expect("remove");
        assert_eq!(answer(&d.0, &ctx, &read_intent_for(&d, "src/lib.rs")), None);
    }

    #[test]
    fn test_f1_answer_read_of_file_outside_graph_is_none() {
        let d = graph_fixture();
        let ctx = d.0.join("sieve");
        assert_eq!(answer(&d.0, &ctx, &read_intent_for(&d, "Cargo.lock")), None);
        assert_eq!(answer(&d.0, &ctx, &read_intent_for(&d, "nope.rs")), None);
        assert_eq!(answer(&d.0, &ctx, &Intent::PassThrough), None);
    }

    #[test]
    fn test_f1_answer_counts_lines_like_the_read_tool() {
        // A final newline ends the last line. It does not start a new one.
        assert_eq!(line_count(b"a\nb\n"), 2);
        assert_eq!(line_count(b"a\nb"), 2);
        assert_eq!(line_count(b""), 0);
        let d = graph_fixture();
        let ctx = d.0.join("sieve");
        // The fixture file ends with a newline: 5 lines, not 6.
        let text = answer(&d.0, &ctx, &read_intent_for(&d, "src/lib.rs")).expect("cached");
        assert!(last_line(&text).contains(" 5 of 5 lines "), "{text}");
        // Now the last byte is not a newline: still 5 lines.
        put(
            &d,
            "src/lib.rs",
            "pub fn alpha() -> u32 {\n    1\n}\n\npub fn beta() {}",
        );
        let text = answer(&d.0, &ctx, &read_intent_for(&d, "src/lib.rs")).expect("cached");
        assert!(last_line(&text).contains(" 5 of 5 lines "), "{text}");
    }

    #[test]
    fn test_f1_bash_note_names_sed_and_the_real_path() {
        let d = graph_fixture();
        let ctx = d.0.join("sieve");
        let intent = read_intent_for(&d, "src/lib.rs");
        let path = d.0.join("src/lib.rs").to_string_lossy().into_owned();
        let bash = answer_bash(&d.0, &ctx, &intent).expect("bash answer");
        let note = last_line(&bash);
        assert!(note.contains("sed -n '6,200p'"), "{note}");
        assert!(note.contains(&path), "{note}");
        assert!(note.contains("5 of 5 lines"), "{note}");
        assert!(note.contains("hook replaced your command"), "{note}");
        // The note names two routes: `sed -n` and the Read tool.
        assert!(note.contains("or Read with offset and limit"), "{note}");
        let read = answer(&d.0, &ctx, &intent).expect("read answer");
        let read_note = last_line(&read);
        assert!(read_note.contains("offset and limit"), "{read_note}");
        assert!(!read_note.contains("sed -n"), "{read_note}");
        for n in [note, read_note] {
            assert!(n.split_whitespace().count() < 30, "{n}");
        }
    }

    #[test]
    fn test_f1_answer_note_is_under_30_words() {
        let d = graph_fixture();
        let ctx = d.0.join("sieve");
        let text = answer(&d.0, &ctx, &read_intent_for(&d, "src/lib.rs")).expect("answer");
        let words = last_line(&text).split_whitespace().count();
        assert!(words < 30, "{words} words: {}", last_line(&text));
    }

    fn text_of(n: u64) -> Option<String> {
        Some("x".repeat(usize::try_from(n).expect("small")))
    }

    fn decide_read(raw: Option<u64>, ans: Option<String>) -> Option<String> {
        decide(&size_only(), raw, ans)
    }

    fn size_only() -> Intent {
        Intent::ReadFile {
            path: "a.rs".into(),
            raw_bytes: 0,
            bounded: false,
        }
    }

    #[test]
    fn test_f1_decide_large_file_small_skeleton_intercepts() {
        assert_eq!(decide_read(Some(20_000), text_of(500)), text_of(500));
    }

    #[test]
    fn test_f1_decide_small_file_similar_skeleton_passes_through() {
        assert_eq!(decide_read(Some(300), text_of(280)), None);
    }

    #[test]
    fn test_f1_decide_no_answer_passes_through() {
        assert_eq!(decide_read(Some(20_000), None), None);
    }

    #[test]
    fn test_f1_decide_empty_answer_never_wins() {
        // 40 bytes is the "no definitions indexed" size. It would pass the
        // ratio and the floor against 20 KB, so only the minimum size stops it.
        assert_eq!(decide_read(Some(20_000), text_of(40)), None);
        assert_eq!(decide_read(Some(20_000), text_of(0)), None);
        assert_eq!(
            decide_read(Some(20_000), text_of(MIN_ANSWER_BYTES)).map(|t| t.len()),
            Some(64)
        );
    }

    #[test]
    fn test_f1_decide_below_the_floor_passes_through_despite_the_ratio() {
        // The live probe: 8,180 raw bytes against a 2,020-byte skeleton.
        assert_eq!(decide_read(Some(8_180), text_of(2_020)), None);
        // One byte under the floor with the best possible ratio.
        assert_eq!(
            decide_read(Some(MIN_RAW_BYTES - 1), text_of(MIN_ANSWER_BYTES)),
            None
        );
    }

    #[test]
    fn test_f1_decide_at_the_floor_with_a_good_ratio_intercepts() {
        assert_eq!(decide_read(Some(MIN_RAW_BYTES), text_of(500)), text_of(500));
    }

    #[test]
    fn test_f1_decide_unmeasured_raw_passes_through() {
        assert_eq!(decide_read(None, text_of(500)), None);
    }

    #[test]
    fn test_f1_decide_saving_just_under_and_over_the_ratio() {
        // Both raw costs sit above the floor, so only the ratio decides.
        let ans = 10_000;
        let edge = ans * MIN_SAVING_FACTOR;
        assert_eq!(decide_read(Some(edge - 1), text_of(ans)), None);
        assert_eq!(decide_read(Some(edge), text_of(ans)), text_of(ans));
    }

    #[test]
    fn test_f1_decide_smaller_by_one_byte_passes_through() {
        // A rule of only "smaller wins" would intercept here. The raw cost
        // is above the floor, so the ratio gate is what stops it.
        assert_eq!(decide_read(Some(20_001), text_of(20_000)), None);
    }

    #[test]
    fn test_f1_decide_never_returns_an_answer_at_or_over_raw_cost() {
        for raw in [64, 100, 500, 5_000] {
            for ans in [64, 100, 500, 5_000, 9_000] {
                if let Some(t) = decide_read(Some(raw), text_of(ans)) {
                    assert!((t.len() as u64) < raw, "raw {raw} ans {ans}");
                }
            }
        }
    }

    #[test]
    fn test_f1_decide_pass_through_intent_never_intercepts() {
        assert_eq!(
            decide(&Intent::PassThrough, Some(20_000), text_of(500)),
            None
        );
    }

    #[test]
    fn test_f1_decide_needs_no_product_env() {
        // `decide` reads no product name, so `SIEVE_PRODUCT` does not matter.
        assert_eq!(decide_read(Some(20_000), text_of(500)), text_of(500));
    }

    #[test]
    fn test_s3_f1_lookup_equals_full() {
        let d = graph_fixture();
        let ctx = d.0.join("sieve");
        let graph = crate::hook::read_wiring(&ctx).expect("graph");
        sieve_parse::write_wiring_and_lookup(&graph, &ctx).expect("write graph");
        let rels = ["src/lib.rs", "Cargo.lock", "src/none.rs"];
        let answers = |bash: bool| -> Vec<Option<String>> {
            rels.iter()
                .map(|rel| {
                    let intent = read_intent_for(&d, rel);
                    if bash {
                        answer_bash(&d.0, &ctx, &intent)
                    } else {
                        answer(&d.0, &ctx, &intent)
                    }
                })
                .collect()
        };
        let (with, with_bash) = (answers(false), answers(true));
        assert!(with[0].is_some(), "the skeleton answers");
        assert!(with[1].is_none() && with[2].is_none());
        crate::hook::s3_support::garble_wiring(&ctx);
        assert_eq!(answers(false), with, "the lookup answers alone");
        crate::hook::s3_support::drop_lookup(&ctx);
        // The garbled wiring cannot serve the full path, so rebuild it.
        sieve_core::write_graph(&graph, &ctx.join(".graph/wiring.json")).expect("write");
        assert_eq!(answers(false), with);
        assert_eq!(answers(true), with_bash);
    }
}
