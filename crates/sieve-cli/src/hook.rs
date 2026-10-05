//! `sieve hook <event>`: the five Claude Code hook events that the hook shim
//! answers — `session-start`, `prompt`, `post-edit`, `tool-savings`, `stop`
//! (the `hosts-hooks.md` note section 3) — plus `pre-read`, Sieve's own F3
//! event that denies a second read of a file whose content has not changed
//! since the last read.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::intercept;
use sieve_core::askindex::{ask_index_path, read_ask_index};
use sieve_core::product::product;
use sieve_core::wiring::{Edge, Graph, Kind, Node};
use sieve_parse::check::check_graph;
use sieve_parse::refresh::{ensure_fresh_children, ensure_fresh_graph, RefreshOptions};
use sieve_query::ask::{ask, scope_of, AskOptions, AskResult};
use sieve_query::workspace::{federate_ask, load_children, FederateAskOptions};

/// Flags for `sieve hook`.
#[derive(Args, Debug)]
pub struct HookArgs {
    /// The hook event: `session-start`, `prompt`, `post-edit`,
    /// `tool-savings`, `pre-read` (denies an unchanged file's second
    /// read, or narrows a large file), `post-read` (adds the note for a
    /// narrowed read), or `stop`.
    /// Absent, the hook does nothing and exits 0.
    pub event: Option<String>,
}

/// Prompts shorter than this never trigger retrieval (`hosts-hooks.md`
/// section 3, `MIN_PROMPT_CHARS`).
const MIN_PROMPT_CHARS: usize = 12;
/// A pack earns its place only above this name-field match strength, or
/// the broader [`HIGH_FLOOR`] ('s `STRONG_FLOOR`).
const STRONG_FLOOR: f64 = 0.1;
/// The broad-coverage floor that clears the strength gate on its own.
const HIGH_FLOOR: f64 = 0.5;
/// How many weak-match nudges one session may spend (`NUDGE_CAP`).
const NUDGE_CAP: u64 = 2;
/// How many recently-injected pointers the novelty gate remembers.
const INJECTED_POINTERS_CAP: usize = 40;
/// How many source bytes of a transcript's tail [`read_tail`] reads.
const TRANSCRIPT_TAIL_BYTES: u64 = 1024 * 1024;

/// Runs `sieve hook <event>`. Every event exits 0 and prints nothing on a
/// no-op path; a hook must never fail the host session over a metric.
pub fn run(args: &HookArgs, _dir_override: Option<&Path>) -> Result<(), String> {
    // The shim passes `process.argv[2]` to `main`; no event matches no branch.
    let Some(event) = args.event.as_deref() else {
        return Ok(());
    };
    let input = read_stdin_json();
    let project_dir = project_dir_of(&input);
    match event {
        "session-start" => handle_session_start(&project_dir),
        "prompt" => handle_prompt(&input, &project_dir),
        "post-edit" => handle_post_edit(&input, &project_dir),
        // `main`'s `post-edit-sync`: the edit hook, then the Stop hook.
        "post-edit-sync" => {
            handle_post_edit(&input, &project_dir);
            handle_stop(&input, &project_dir);
        }
        "tool-savings" => handle_tool_use(&input, &project_dir),
        "cursor-post-tool" => handle_cursor_post_tool(&input, &project_dir),
        "cursor-mcp" => handle_cursor_mcp(&input, &project_dir),
        // Sieve force-closes the conversation into a `session_summary`
        // telemetry event here. Sieve sends no telemetry (P4-35 to P4-38 are
        // deviations), so the event has nothing to do.
        "cursor-session-end" => {}
        "pre-read" => {
            let text = if is_bash_call(&input) {
                handle_pre_bash(&input, &project_dir)
            } else {
                handle_pre_read(&input, &project_dir)
            };
            if let Some(text) = text {
                print!("{text}");
            }
        }
        "post-read" => {
            let text = if is_bash_call(&input) {
                handle_post_bash(&input, &project_dir)
            } else {
                handle_post_read(&input, &project_dir)
            };
            if let Some(text) = text {
                print!("{text}");
            }
        }
        "stop" => handle_stop(&input, &project_dir),
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stdin, project dir, context dir
// ---------------------------------------------------------------------------

/// Reads the hook's stdin JSON. `SIEVE_TEST_STDIN` is a test seam that
/// stands in for fd 0 (`hooks.ts`'s `readStdin`). A parse failure, or a
/// missing seam and an unreadable fd 0, both become `{}`.
fn read_stdin_json() -> Value {
    let raw = match std::env::var(product().env_var("TEST_STDIN")) {
        Ok(v) => v,
        Err(_) => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }
    };
    serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(Default::default()))
}

/// The project dir a hook runs against: `CLAUDE_PROJECT_DIR`, else the
/// stdin payload's `cwd`, else the process cwd (`hooks.ts`'s
/// `projectDir`). An empty env var counts as unset, matching the `||`
/// chain Sieve uses.
fn project_dir_of(input: &Value) -> PathBuf {
    if let Ok(v) = std::env::var("CLAUDE_PROJECT_DIR") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Some(cwd) = input.get("cwd").and_then(Value::as_str) {
        if !cwd.is_empty() {
            return PathBuf::from(cwd);
        }
    }
    std::env::current_dir().unwrap_or_default()
}

/// The context dir for a project dir: `SIEVE_DIR` when set, else
/// `<project_dir>/sieve`.
pub fn resolve_context_dir(project_dir: &Path) -> PathBuf {
    if let Ok(v) = std::env::var(product().env_var("DIR")) {
        if !v.is_empty() {
            let p = PathBuf::from(&v);
            return if p.is_absolute() {
                p
            } else {
                project_dir.join(p)
            };
        }
    }
    project_dir.join(product().context_dir_name())
}

fn cache_dir(project_dir: &Path) -> PathBuf {
    resolve_context_dir(project_dir).join(".cache")
}

fn wiring_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".graph").join("wiring.json")
}

pub(crate) fn read_wiring(context_dir: &Path) -> Option<Graph> {
    let bytes = std::fs::read(wiring_path(context_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Emits one hook response line: compact JSON, no trailing newline
/// (`hooks.ts`'s `emit`). A struct, not a `serde_json::Value`, so the key
/// order (`hookEventName` before `additionalContext`) survives — this
/// workspace's `serde_json` has no `preserve_order` feature, so a `Value`
/// object would otherwise serialize its keys alphabetically.
fn emit(event_name: &str, additional_context: &str) {
    if let Some(text) = context_json(event_name, additional_context) {
        print!("{text}");
    }
}

/// Builds the one-line `additionalContext` payload that [`emit`] prints.
fn context_json(event_name: &str, additional_context: &str) -> Option<String> {
    #[derive(Serialize)]
    struct HookOutput<'a> {
        #[serde(rename = "hookEventName")]
        event: &'a str,
        #[serde(rename = "additionalContext")]
        additional_context: &'a str,
    }
    #[derive(Serialize)]
    struct EmitPayload<'a> {
        #[serde(rename = "hookSpecificOutput")]
        hook_specific_output: HookOutput<'a>,
    }
    let payload = EmitPayload {
        hook_specific_output: HookOutput {
            event: event_name,
            additional_context,
        },
    };
    serde_json::to_string(&payload).ok()
}

// ---------------------------------------------------------------------------
// The `sieve/.cache/stats.json` and `sieve/.cache/session/<id>.json` state
// ---------------------------------------------------------------------------

/// The statusline's cached snapshot ('s `Stats`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Stats {
    #[serde(rename = "nodeCount")]
    pub node_count: u64,
    #[serde(rename = "edgeCount")]
    pub edge_count: u64,
    pub languages: Vec<String>,
    #[serde(rename = "totalCount")]
    pub total_count: u64,
    #[serde(rename = "readyCount")]
    pub ready_count: u64,
    #[serde(rename = "staleCount")]
    pub stale_count: u64,
    pub dirty: bool,
    pub syncing: bool,
    #[serde(rename = "syncedAt")]
    pub synced_at: Option<String>,
    #[serde(rename = "lastFile")]
    pub last_file: Option<String>,
    /// The count of F1 notes that never reached the agent (the `narrowed`
    /// entries left in a session at `stop`). Absent at zero, so the Phase 1
    /// `stats.json` golden does not change.
    #[serde(rename = "notesLost", default, skip_serializing_if = "is_zero")]
    pub notes_lost: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// One Claude Code session's usage state. Field order is the insertion
/// order of the fields the goldens exercise: `host` is stamped before
/// `turnUsedSieve` in the one call sequence the parity test drives.
/// ponytail: a fixed declaration order stands in for the JS object's
/// true dynamic insertion order; upgrade to an ordered map if a future
/// golden needs a different sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionState {
    #[serde(rename = "lastQuery")]
    pub last_query: Option<String>,
    #[serde(rename = "perAgentQuery", default)]
    pub per_agent_query: BTreeMap<String, String>,
    #[serde(rename = "toolReads", default)]
    pub tool_reads: u64,
    #[serde(rename = "sourceReads", default)]
    pub source_reads: u64,
    #[serde(rename = "savedTokens", default)]
    pub saved_tokens: u64,
    #[serde(rename = "injectedPointers", default)]
    pub injected_pointers: Vec<String>,
    #[serde(default)]
    pub nudges: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub host: Option<String>,
    #[serde(
        rename = "turnUsedSieve",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub turn_used_sieve: Option<bool>,
    #[serde(
        rename = "sieveTurns",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub sieve_turns: Option<u64>,
    #[serde(
        rename = "reportedTurns",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub reported_turns: Option<u64>,
    #[serde(
        rename = "lastTallyUuid",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub last_tally_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub summarized: Option<bool>,
    /// The billed input cost this session has accumulated, in micro-dollars.
    /// The stop hook sums it from the transcript; Sieve keeps the value a host
    /// wrote, so a session rewrite never drops it and the statusline can price
    /// the saving.
    #[serde(
        rename = "inputCostMicros",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub input_cost_micros: Option<serde_json::Number>,
    /// Every input token this session was billed for, the denominator of
    /// the blended rate. Kept the same way.
    #[serde(
        rename = "inputTokensBilled",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub input_tokens_billed: Option<serde_json::Number>,
    /// The tokens saved per UTC day, `YYYY-MM-DD` to tokens. Written under
    /// the sieve product only, so the goldens do not change. `sieve
    /// stats --json` sums it into the today and 7-day totals.
    #[serde(
        rename = "savedByDay",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub saved_by_day: BTreeMap<String, u64>,
    /// One record per file path read this session (F3, "remembered
    /// reads"). Empty on every Phase 1 session, so `skip_serializing_if`
    /// keeps `state.json` byte-identical to the Phase 1 golden.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reads: BTreeMap<String, ReadRecord>,
    /// The note text for each Read that F1 narrowed and `post-read` has
    /// not yet answered, by canonical path. Empty on every Phase 1
    /// session, so the golden `state.json` does not change.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub narrowed: BTreeMap<String, String>,
}

impl SessionState {
    /// Adds `n` saved tokens to the session total and to today's local-day
    /// total.
    fn add_saved(&mut self, n: u64) {
        self.saved_tokens += n;
        *self
            .saved_by_day
            .entry(crate::telemetry::today_local())
            .or_insert(0) += n;
    }
}

/// One file's remembered read: the content hash seen last, and whether
/// that read already earned this session's one denial (F3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReadRecord {
    pub hash: String,
    #[serde(default)]
    pub denied: bool,
}

/// The longest session file stem, in bytes.
const SESSION_ID_MAX: usize = 128;

/// The file stem for a session id. A normal id (`[A-Za-z0-9._-]+`, not `.`
/// or `..`, at most 128 bytes) is kept, so the file names stay. Any
/// other id is made safe: each outside byte becomes `_`, an empty or
/// dot-leading result gets a `_` prefix, and the result is cut to 128 bytes.
/// This is a security deviation: the `sessionPath` joins the raw id, so
/// `../../x` leaves the session dir.
fn safe_session_stem(id: &str) -> String {
    let ok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-');
    if !id.is_empty() && id != "." && id != ".." && id.len() <= SESSION_ID_MAX && id.bytes().all(ok)
    {
        return id.to_string();
    }
    let mut stem: String = id
        .bytes()
        .map(|b| if ok(b) { b as char } else { '_' })
        .collect();
    if stem.is_empty() || stem.starts_with('.') {
        stem.insert(0, '_');
    }
    stem.truncate(SESSION_ID_MAX);
    stem
}

fn session_path(project_dir: &Path, id: &str) -> PathBuf {
    let dir = cache_dir(project_dir).join("session");
    let path = dir.join(format!("{}.json", safe_session_stem(id)));
    // Defence in depth: the path must be a direct child of the session dir.
    if path.parent() == Some(dir.as_path()) {
        path
    } else {
        dir.join("default.json")
    }
}

fn stats_path(project_dir: &Path) -> PathBuf {
    cache_dir(project_dir).join("stats.json")
}

pub(crate) fn read_session(project_dir: &Path, id: &str) -> SessionState {
    std::fs::read(session_path(project_dir, id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_session(project_dir: &Path, id: &str, state: &SessionState) -> std::io::Result<()> {
    write_json_atomic(&session_path(project_dir, id), state)
}

/// Waits this long between two tries of the session lock.
const SESSION_LOCK_POLL_MS: u64 = 10;
/// Stops waiting for the session lock after this many milliseconds in total.
const SESSION_LOCK_WAIT_MS: u64 = 200;

/// Waits for the lock in `lock_dir`, for up to `SESSION_LOCK_WAIT_MS`.
///
/// The function tries every `SESSION_LOCK_POLL_MS`. It returns `None` when
/// the wait ends or when an IO error occurs. The caller then runs unlocked,
/// so a hook never hangs the host session.
///
/// `lock_path` always appends `.cache/.sync.lock` to `lock_dir`. So the lock
/// file sits in `<lock_dir>/.cache/`, and the call leaves that empty
/// directory behind.
fn acquire_hook_lock(lock_dir: &Path) -> Option<sieve_core::lock::LockGuard> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(SESSION_LOCK_WAIT_MS);
    loop {
        match sieve_core::lock::LockGuard::acquire(lock_dir) {
            Ok(Some(guard)) => return Some(guard),
            Err(_) => return None,
            Ok(None) if std::time::Instant::now() >= deadline => return None,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(SESSION_LOCK_POLL_MS)),
        }
    }
}

/// Reads the session, runs `change` on it, and writes it back under a lock.
///
/// Two parallel hook calls must not lose each other's update. The function
/// tries the lock every 10 ms, for up to 200 ms in total. If it still has no
/// lock, it runs unlocked, so a hook never hangs the host session. An IO
/// error is not contention: on an error the function stops waiting at once
/// and runs unlocked.
///
/// Cost: the read-modify-write takes microseconds, so a real wait almost
/// never exceeds one retry. If a killed holder leaves a stale lock, every
/// hook pays up to 200 ms until `LOCK_STALE_MS` clears it, which is 300
/// seconds.
fn update_session<R>(
    project_dir: &Path,
    id: &str,
    change: impl FnOnce(&mut SessionState) -> R,
) -> R {
    update_session_checked(project_dir, id, change).0
}

/// [`update_session`], and whether the write to disk succeeded. F1 needs
/// the flag: a narrowed Read with no record on disk gets no note.
fn update_session_checked<R>(
    project_dir: &Path,
    id: &str,
    change: impl FnOnce(&mut SessionState) -> R,
) -> (R, bool) {
    // The session directory keeps this lock apart from the graph build lock
    // and from the stats lock.
    let lock_dir = session_path(project_dir, id)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let _guard = acquire_hook_lock(&lock_dir);
    let mut state = read_session(project_dir, id);
    let out = change(&mut state);
    let written = write_session(project_dir, id, &state).is_ok();
    (out, written)
}

pub(crate) fn read_stats(project_dir: &Path) -> Option<Stats> {
    std::fs::read(stats_path(project_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// Read-modify-write over `stats.json` under a lock, so two parallel hook
/// calls do not lose each other's update (F3).
fn patch_stats(project_dir: &Path, patch: impl FnOnce(&mut Stats)) {
    // The lock directory is the cache directory itself. So the lock file is
    // `<ctx>/.cache/.cache/.sync.lock`. This differs from the session lock
    // (`<ctx>/.cache/session/.cache/.sync.lock`) and from the graph build
    // lock (`<ctx>/.cache/.sync.lock`), so a hook never waits for a rebuild.
    let _guard = acquire_hook_lock(&cache_dir(project_dir));
    let mut next = read_stats(project_dir).unwrap_or_default();
    patch(&mut next);
    let _ = write_json_atomic(&stats_path(project_dir), &next);
}

/// Writes `value` as 2-space JSON via a scratch file and a rename
/// (`writeJsonAtomic`).
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_file_name(format!(
        "{}.{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id(),
        format!("{:?}", std::thread::current().id())
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
    ));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// session-start
// ---------------------------------------------------------------------------

/// How many of a manifest's recorded source files are missing from disk
/// ('s `indexFreshness`, `missing` and `total`).
struct IndexFreshness {
    missing: usize,
    total: usize,
}

/// Counts recorded source files that no longer exist on disk
/// (`indexFreshness`). Returns `None` when
/// `context_dir` carries no `manifest.json` — the "no deep layer" case,
/// same as `Manifest::read`.
fn index_freshness(project_dir: &Path, context_dir: &Path) -> Option<IndexFreshness> {
    let manifest = sieve_core::concept::Manifest::read(context_dir)?;
    let missing = manifest
        .files
        .iter()
        .filter(|f| !project_dir.join(&f.path).exists())
        .count();
    Some(IndexFreshness {
        missing,
        total: manifest.files.len(),
    })
}

/// One-line staleness banner, or `None` when the index is fresh or absent
/// ('s `staleBanner`).
fn stale_banner(freshness: Option<&IndexFreshness>) -> Option<String> {
    let f = freshness?;
    if f.missing == 0 {
        return None;
    }
    Some(format!(
        "\u{26a0} sieve's index may be ahead of your working tree: {} of {} indexed \
        files are not on disk (branch switch or uncommitted move?). If sieve names a path that \
        isn't there, don't chase it \u{2014} `sieve grep` the symbol to find where it lives now; \
        run `sieve build` to refresh.",
        f.missing, f.total
    ))
}

/// Renders the `SessionStart` orientation: the directive, a blank line,
/// `repo map (sieve/INDEX.md):`, then the first `budget_units` UTF-16
/// code units of `INDEX.md` (`formatOrientation`'s `slice(0, budgetBytes)`,
/// a JS slice, so units, not bytes). `stale_note` is the banner from
/// [`stale_banner`], or `None` when there is no manifest, or the manifest
/// names no missing file.
fn format_orientation(index_md: &str, budget_units: usize, stale_note: Option<&str>) -> String {
    let banner = match stale_note {
        Some(note) => format!("{note}\n\n"),
        None => String::new(),
    };
    let head = utf16_prefix(index_md, budget_units);
    let directive = crate::templates::SESSION_START;
    let context_dir = product().context_dir_name();
    format!("{banner}{directive}\nrepo map ({context_dir}/INDEX.md):\n{head}")
}

/// The longest prefix of `s` that fits in `units` UTF-16 code units: JS
/// `s.slice(0, units)`, the count every `.length`/`.slice` port must use.
/// ponytail: a cut inside a surrogate pair drops the whole pair; JS keeps
/// the lone high surrogate and prints it as `\ud83d`, which a `str` cannot
/// hold. Match it in `emit` if a golden ever lands an emoji on the cut.
fn utf16_prefix(s: &str, units: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += c.len_utf16();
        if used > units {
            return &s[..i];
        }
    }
    s
}

fn handle_session_start(project_dir: &Path) {
    let context_dir = resolve_context_dir(project_dir);
    // Sieve ships no telemetry and no update nudge (LEDGER.md, 2026-09-13,
    // "Sieve ships no telemetry in Phase 1").
    // No INDEX.md (never built here): nothing to say.
    if let Ok(index_md) = std::fs::read_to_string(context_dir.join("INDEX.md")) {
        let freshness = index_freshness(project_dir, &context_dir);
        let banner = stale_banner(freshness.as_ref());
        let orientation = format_orientation(&index_md, 1500, banner.as_deref());
        emit("SessionStart", &orientation);
    }
}

// ---------------------------------------------------------------------------
// prompt
// ---------------------------------------------------------------------------

fn handle_prompt(input: &Value, project_dir: &Path) {
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    // JS `.length`: UTF-16 code units, not chars (six emoji pass the floor).
    if prompt.encode_utf16().count() < MIN_PROMPT_CHARS {
        return;
    }

    let context_dir = resolve_context_dir(project_dir);
    let last_file = read_stats(project_dir).and_then(|s| s.last_file);
    let scope_hint = last_file_scope_hint(&context_dir, last_file.as_deref());

    let Some(result) = run_ask_for_prompt(project_dir, &context_dir, &prompt, scope_hint) else {
        return;
    };

    let id = session_id_of(input);
    let txt = update_session(project_dir, &id, |session| {
        session.last_query = Some(prompt.clone());
        if let Some(agent) = input
            .get("agent")
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
        {
            if !agent.is_empty() {
                session
                    .per_agent_query
                    .insert(agent.to_string(), prompt.clone());
            }
        }
        relevant_retrieval(&result, session, &prompt, 3)
    });
    if let Some(txt) = &txt {
        emit("UserPromptSubmit", txt);
    }
}

fn session_id_of(input: &Value) -> String {
    input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string()
}

/// Runs the in-process equivalent of `sieve ask "<prompt>" . --json -n 3`
/// (`--in <scope>` too, when `in_prefix` is set). Refreshes the graph
/// first, discarding the refresh note — the real hook spawns `sieve ask`
/// with its stderr piped to `/dev/null`, so this child's stderr must stay
/// silent too. Returns `None` for a
/// failed call: no graph to read.
fn run_ask_for_prompt(
    project_dir: &Path,
    context_dir: &Path,
    prompt: &str,
    in_prefix: Option<String>,
) -> Option<AskResult> {
    let opts = RefreshOptions {
        disabled: false,
        force_hash: false,
    };
    // At a workspace parent `sieve ask` federates, and
    // its prelude refreshes each child (P4-46). A federated error, such as
    // an unknown `--in` child, is a failed call too.
    if let Some(ws) = sieve_core::workspace::read(context_dir) {
        let _ = ensure_fresh_children(project_dir, &ws.children, &opts);
        let graphs = load_children(project_dir, context_dir);
        let fed = FederateAskOptions {
            limit: 3,
            source: false,
            full: false,
            in_prefix,
        };
        return federate_ask(&graphs, prompt, &fed).ok();
    }
    let _ = ensure_fresh_graph(project_dir, context_dir, &opts);
    let graph = read_wiring(context_dir)?;
    let index = read_ask_index(&ask_index_path(context_dir));
    let ask_opts = AskOptions {
        limit: 3,
        in_prefix,
        graph_rank: true,
        source: false,
        full: false,
    };
    ask(&graph, index.as_ref(), prompt, &ask_opts, project_dir).ok()
}

/// The "you're working in backend/, weight it" hint (`lastFileScopeHint`):
/// on a multi-scope graph, the scope of the last-edited file. `last_file`
/// is a basename, so this matches any file node whose path is, or ends in
/// `/`, that name. Every miss fails soft with `None`: no graph, one scope,
/// a basename no longer in the graph, a basename in more than one scope,
/// or a root-scope file. The two graph misses print one stderr note each,
/// the same text the CLI prints. The `catch` note ("scope hint lookup
/// failed") has no Rust path: `read_wiring` returns `None` instead of
/// throwing, and that is the no-graph branch.
fn last_file_scope_hint(context_dir: &Path, last_file: Option<&str>) -> Option<String> {
    let last_file = last_file?;
    let graph = read_wiring(context_dir)?;
    let scopes = &graph.meta.scopes;
    if scopes.len() <= 1 {
        return None;
    }
    let suffix = format!("/{last_file}");
    let prefixes: HashSet<String> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File && (n.path == last_file || n.path.ends_with(&suffix)))
        .map(|n| scope_of(&n.path, scopes))
        .collect();
    if prefixes.is_empty() {
        eprintln!("[sieve] prompt hook: lastFile \"{last_file}\" not found in the graph \u{2014} skipping scope hint");
        return None;
    }
    if prefixes.len() > 1 {
        eprintln!("[sieve] prompt hook: lastFile \"{last_file}\" matches more than one scope \u{2014} skipping scope hint");
        return None;
    }
    prefixes.into_iter().next().filter(|p| !p.is_empty())
}

/// The per-prompt injection gate (`relevantRetrieval`). Mutates `session`
/// to remember what was shown; the caller persists it.
fn relevant_retrieval(
    result: &AskResult,
    session: &mut SessionState,
    prompt: &str,
    cap: usize,
) -> Option<String> {
    if result.hits.is_empty() {
        return None;
    }
    //: the gate runs when either coverage is a
    // number; an absent one counts as `0`. A structural result carries
    // neither, so it always passes.
    let lexical = result.coverage.is_some() || result.coverage_strong.is_some();
    if lexical {
        let strong = result.coverage_strong.unwrap_or(0.0);
        let broad = result.coverage.unwrap_or(0.0);
        if strong < STRONG_FLOOR && broad < HIGH_FLOOR {
            return weak_match_nudge(session, prompt);
        }
    }
    let seen: HashSet<&str> = session
        .injected_pointers
        .iter()
        .map(String::as_str)
        .collect();
    let fresh: Vec<&sieve_query::ask::AskHit> = result
        .hits
        .iter()
        .filter(|h| !seen.contains(h.pointer.as_str()))
        .collect();
    if fresh.is_empty() {
        return None;
    }
    let txt = format_retrieval(&fresh, cap);
    let newly: Vec<String> = fresh.iter().take(cap).map(|h| h.pointer.clone()).collect();
    session.injected_pointers.extend(newly);
    let len = session.injected_pointers.len();
    if len > INJECTED_POINTERS_CAP {
        session
            .injected_pointers
            .drain(0..len - INJECTED_POINTERS_CAP);
    }
    Some(txt)
}

/// The weak-match nudge, spent at most [`NUDGE_CAP`] times per session
/// (`weakMatchNudge`).
fn weak_match_nudge(session: &mut SessionState, prompt: &str) -> Option<String> {
    if session.nudges >= NUDGE_CAP {
        return None;
    }
    session.nudges += 1;
    Some(crate::templates::prompt_hint(prompt).to_string())
}

/// The retrieval pack (`formatRetrieval`). `ask.saved` — the whole-file
/// baseline `formatRetrieval` would append a savings footer from — is
/// only ever set by `ask --source`, and the prompt hook never passes
/// `--source` (per-prompt injected tokens stay pointers-only). So the
/// footer branch never fires here; this renders the pack body alone.
fn format_retrieval(hits: &[&sieve_query::ask::AskHit], cap: usize) -> String {
    let hits: Vec<&sieve_query::ask::AskHit> = hits.iter().take(cap).copied().collect();
    retrieval_body(&hits)
}

fn retrieval_body(hits: &[&sieve_query::ask::AskHit]) -> String {
    let blocks: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let ptr = h.pointer.split(',').next().unwrap_or("").trim();
            let snippet = collapse_whitespace(&h.snippet);
            // `slice(0, 140)`: UTF-16 units, not chars.
            let snippet = utf16_prefix(&snippet, 140);
            let mut block = format!(" {}. {}: {}", i + 1, h.title, ptr);
            if !snippet.is_empty() {
                block.push_str(&format!("\n    {snippet}"));
            }
            if let Some(code) = &h.code {
                block.push_str(&format!("\n```\n{code}\n```"));
            }
            block
        })
        .collect();
    let header = if hits.iter().any(|h| h.code.is_some()) {
        "[sieve] retrieved context, read these spans; do not re-open the files:"
    } else {
        "[sieve] starting points for this task: pull the code inline with `sieve ask \"<what you \
        need>\" --source`, trace impact with `sieve callers <symbol>`, or search with `sieve grep \
        \"<literal>\"`:"
    };
    format!("{}\n{}", header, blocks.join("\n"))
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// post-edit
// ---------------------------------------------------------------------------

fn handle_post_edit(input: &Value, project_dir: &Path) {
    let Some(file) = edited_file_path(input, project_dir) else {
        return;
    };
    if under_sieve(project_dir, &file) {
        return;
    }
    let context_dir = resolve_context_dir(project_dir);
    let stale = check_stale_count(project_dir, &context_dir);
    let base = basename(&file);
    patch_stats(project_dir, |s| {
        s.dirty = true;
        s.stale_count = stale;
        s.last_file = Some(base.clone());
    });
    if let Some(graph) = read_wiring(&context_dir) {
        if let Some(blast) = format_blast_radius(&graph, &file, 8) {
            emit("PostToolUse", &blast);
        }
    }
}

/// The absolute path of the file a `PostToolUse` edit touched
/// (`editedFilePath`): `tool_input.file_path` directly, else the first
/// `*** Add File:`/`*** Update File:` line of `tool_input.command`,
/// resolved against `dir`.
fn edited_file_path(input: &Value, dir: &Path) -> Option<String> {
    if let Some(direct) = input
        .get("tool_input")
        .and_then(|t| t.get("file_path"))
        .and_then(Value::as_str)
    {
        if !direct.trim().is_empty() {
            return Some(direct.to_string());
        }
    }
    let command = input
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(Value::as_str)?;
    for line in command.lines() {
        let trimmed = line.trim();
        for prefix in ["*** Add File: ", "*** Update File: "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let rest = rest.trim_end();
                let candidate = Path::new(rest);
                return Some(if candidate.is_absolute() {
                    rest.to_string()
                } else {
                    dir.join(rest).to_string_lossy().into_owned()
                });
            }
        }
    }
    None
}

/// Whether `file` sits under this repo's `sieve/` tree.
fn under_sieve(dir: &Path, file: &str) -> bool {
    let dir_str = dir.to_string_lossy();
    let rel = file.strip_prefix(dir_str.as_ref()).unwrap_or(file);
    let rel = rel.trim_start_matches(['/', '\\']);
    rel.replace('\\', "/")
        .starts_with(&format!("{}/", product().context_dir_name()))
}

fn check_stale_count(root: &Path, context_dir: &Path) -> u64 {
    match check_graph(root, context_dir) {
        Ok(g) if !g.missing => (g.changed.len() + g.added.len() + g.removed.len()) as u64,
        _ => 0,
    }
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn node_ids_in_file<'a>(graph: &'a Graph, file_path: &str) -> HashSet<&'a str> {
    graph
        .nodes
        .iter()
        .filter(|n| {
            !n.path.is_empty()
                && (file_path == n.path || file_path.ends_with(&format!("/{}", n.path)))
        })
        .map(|n| n.id.as_str())
        .collect()
}

fn incoming_edges<'a>(graph: &'a Graph, file_path: &str) -> Vec<&'a Edge> {
    let ids = node_ids_in_file(graph, file_path);
    if ids.is_empty() {
        return Vec::new();
    }
    graph
        .edges
        .iter()
        .filter(|e| ids.contains(e.target.as_str()) && !ids.contains(e.source.as_str()))
        .collect()
}

/// The blast-radius text for one edited file's incoming edges
/// (`formatBlastRadius`), capped at `cap`, or `None` when it has none.
fn format_blast_radius(graph: &Graph, file_path: &str, cap: usize) -> Option<String> {
    let edges = incoming_edges(graph, file_path);
    if edges.is_empty() {
        return None;
    }
    let by_id: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let items: Vec<String> = edges
        .iter()
        .take(cap)
        .map(|e| {
            let label = match by_id.get(e.source.as_str()) {
                Some(n) => format!("{} ({})", n.name, basename(&n.path)),
                None => e.source.clone(),
            };
            format!(" \u{2022} {} \u{2190} {label}", e.relation.as_str())
        })
        .collect();
    let more = if edges.len() > cap {
        format!("\n \u{2022} +{} more", edges.len() - cap)
    } else {
        String::new()
    };
    Some(format!(
        "{} {}, who depends on it:\n{}{more}",
        "[sieve] blast radius for",
        basename(file_path),
        items.join("\n")
    ))
}

// ---------------------------------------------------------------------------
// pre-read (F3, "remembered reads", then F1, "narrow a large Read")
// ---------------------------------------------------------------------------

/// Denies a second read of a file whose bytes have not changed since the
/// last read this session. Returns the `PreToolUse` deny JSON to print, or
/// `None` on the pass-through path (first read, changed file, or the one
/// escape read after a deny). The first read of a path, and any read after
/// a change, always passes. A denied read earns one escape: the read right
/// after a deny always passes too, so a host that compacts the deny
/// message out of context never leaves the agent stuck.
///
/// This handler reads `session_id` from the stdin JSON directly, instead
/// of `session_id_of`'s `"default"` fallback: two sessions that both send
/// no session id would otherwise share one record file, and one session's
/// read could deny the other session's first read of the same path. A
/// missing, non-string, or empty session id skips this handler entirely.
///
/// F1 runs after F3. F1 narrows a large Read to `NARROWED_LINES` when the
/// cached skeleton is smaller. The two features share one rule. An F3
/// record claims that the agent holds the file content. A narrowed Read
/// does not deliver that content. So F1 must remove F3's record when F1
/// narrows. Otherwise F3 denies the next read with a false claim, and the
/// span read that the note names gets the same false deny.
///
/// F1 records the note text in the session before it returns the allow
/// JSON. If that record does not reach the disk, F1 passes the read
/// through, so a narrowed Read never goes without its `post-read` note.
/// Every call first clears the old note for the path, so a full read
/// never gets a stale note from an earlier narrowing.
fn handle_pre_read(input: &Value, project_dir: &Path) -> Option<String> {
    let file_path = input
        .get("tool_input")
        .and_then(|t| t.get("file_path"))
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())?;
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let bytes = std::fs::read(file_path).ok()?;
    // Canonicalizes the path so an absolute read and a relative read of
    // the same file share one record; a raw `file_path` key would miss
    // between the two shapes and never deny the second read.
    let canonical = std::fs::canonicalize(file_path).ok()?;
    let key = canonical.to_string_lossy().into_owned();
    let hash = sieve_core::fingerprint::hash_source(&bytes);
    let deny = update_session(project_dir, session_id, |session| {
        // Every read clears the old F1 note. Only a narrowing below writes a
        // new one. So `post-read` never sends a note for a full read.
        session.narrowed.remove(&key);
        let unchanged_and_not_yet_denied = session
            .reads
            .get(&key)
            .is_some_and(|r| r.hash == hash && !r.denied);
        if unchanged_and_not_yet_denied {
            session
                .reads
                .insert(key.clone(), ReadRecord { hash, denied: true });
            session.add_saved(bytes.len() as u64 / 4);
            return true;
        }
        // ponytail: the deny alternates by design, one deny per two reads. The
        // deny text names the escape, so the agent spends it only when it lost
        // the copy, not on every unchanged read.
        session.reads.insert(
            key.clone(),
            ReadRecord {
                hash,
                denied: false,
            },
        );
        false
    });
    if deny {
        return Some(pre_read_deny_json(file_path));
    }
    // Only the Read tool reaches this line. `handle_pre_bash` owns a Bash
    // read. A Grep has no wiring yet.
    let intent = intercept::classify(input);
    let intercept::Intent::ReadFile {
        raw_bytes, bounded, ..
    } = intent
    else {
        return None;
    };
    // A Read with `offset` or `limit` asks for a part of the file. F1 never
    // narrows it. F3 has already run above. The F1 note tells the agent to
    // Read a span, so F1 must not block that span.
    if bounded {
        return None;
    }
    let context_dir = resolve_context_dir(project_dir);
    let graph_answer = intercept::answer(project_dir, &context_dir, &intent);
    let note = intercept::decide(&intent, Some(raw_bytes), graph_answer)?;
    let (_, recorded) = update_session_checked(project_dir, session_id, |session| {
        // F3's record claims that the agent holds the content. F1 did not
        // deliver the content. So F1 removes the record.
        session.reads.remove(&key);
        session.narrowed.insert(key, note);
    });
    recorded.then(|| pre_read_narrow_json(file_path))
}

/// Builds the `PreToolUse` allow payload that narrows the Read to the first
/// `NARROWED_LINES` lines through `updatedInput`.
fn pre_read_narrow_json(file_path: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
                "file_path": file_path,
                "offset": 1,
                "limit": intercept::NARROWED_LINES,
            },
        },
    })
    .to_string()
}

/// Answers a narrowed Read (F1). If `pre-read` recorded a note for this
/// file, the handler returns the `PostToolUse` JSON with the note as
/// `additionalContext` and clears the record. Otherwise it returns `None`.
fn handle_post_read(input: &Value, project_dir: &Path) -> Option<String> {
    let file_path = input
        .get("tool_input")
        .and_then(|t| t.get("file_path"))
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())?;
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let key = std::fs::canonicalize(file_path)
        .ok()?
        .to_string_lossy()
        .into_owned();
    let note = update_session(project_dir, session_id, |session| {
        session.narrowed.remove(&key)
    });
    // F6: one line when the read range holds a node with a decision link.
    let hint = why_read_hint(input, project_dir, file_path);
    let text = match (note, hint) {
        (Some(n), Some(h)) => format!("{n}\n{h}"),
        (Some(t), None) | (None, Some(t)) => t,
        (None, None) => return None,
    };
    context_json("PostToolUse", &text)
}

/// The read range of a Read call, as 1-based lines. `None` means the
/// whole file.
fn read_range(input: &Value) -> Option<(usize, usize)> {
    let tool = input.get("tool_input")?;
    let start = tool.get("offset").and_then(Value::as_u64)?.max(1) as usize;
    let end = tool
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(usize::MAX, |n| start.saturating_add(n as usize));
    Some((start, end))
}

/// The F6 decision hint for a Read of `file_path`, or `None`.
fn why_read_hint(input: &Value, project_dir: &Path, file_path: &str) -> Option<String> {
    let context_dir = resolve_context_dir(project_dir);
    crate::why::read_hint(project_dir, &context_dir, file_path, read_range(input))
}

// ---------------------------------------------------------------------------
// pre-read and post-read for a Bash read (F1, "narrow a large cat")
// ---------------------------------------------------------------------------

/// True when the hook stdin names the Bash tool.
fn is_bash_call(input: &Value) -> bool {
    input.get("tool_name").and_then(Value::as_str) == Some("Bash")
}

/// Narrows a Bash read of one large file (F1). `classify` accepts a `cat`,
/// `head`, `tail`, `sed -n`, `less` or `bat` of one file. It rejects a
/// quote, a variable, a glob and a compound command. The handler rewrites
/// the command to `head -5 '<path>'` through `updatedInput`. The agent then
/// receives five real lines, and `post-read` adds the skeleton note.
///
/// A bounded command passes through, whatever its cost. `head -N`,
/// `tail -N`, `head -c N`, `tail -c N` and `sed -n 'A,Bp'` ask for a part
/// of the file. That is the behaviour F1 wants. Only a wholesale `cat`,
/// `less` or `bat` is a candidate for the rewrite.
///
/// The record comes before the rewrite, as in `handle_pre_read`. If the
/// record does not reach the disk, the command passes through. Every call
/// first clears the old note for the path.
///
/// F3 does not apply here. The F3 deny fires only on the Read tool. A Bash
/// read never creates and never removes an F3 record.
fn handle_pre_bash(input: &Value, project_dir: &Path) -> Option<String> {
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let intent = intercept::classify(input);
    let intercept::Intent::ReadFile {
        path,
        raw_bytes,
        bounded,
    } = &intent
    else {
        return None;
    };
    if *bounded {
        return None;
    }
    let key = std::fs::canonicalize(path)
        .ok()?
        .to_string_lossy()
        .into_owned();
    // The rewritten command single-quotes the key. A key with a single
    // quote has no safe form, so the call passes through.
    if key.contains('\'') {
        return None;
    }
    let context_dir = resolve_context_dir(project_dir);
    let graph_answer = intercept::answer_bash(project_dir, &context_dir, &intent);
    let note = intercept::decide(&intent, Some(*raw_bytes), graph_answer);
    let (narrowed, recorded) = update_session_checked(project_dir, session_id, |session| {
        // Every Bash read clears the old note. Only a narrowing writes a
        // new one. So `post-read` never sends a note for a full read.
        session.narrowed.remove(&key);
        match note {
            Some(text) => {
                session.narrowed.insert(key.clone(), text);
                true
            }
            None => false,
        }
    });
    (narrowed && recorded).then(|| pre_bash_narrow_json(input, &key))
}

/// The rewritten command for a narrowed Bash read: `head -5 '<key>'`. The
/// single quotes keep a space or a shell character in the path literal.
/// `classify` already rejects a command with a quote, a variable or a
/// glob, and `handle_pre_bash` rejects a key with a single quote. So the
/// quoted form is always safe.
fn narrowed_bash_command(key: &str) -> String {
    format!("head -{} '{key}'", intercept::NARROWED_LINES)
}

/// Reads the canonical key back from a command `narrowed_bash_command`
/// built. `PostToolUse` receives the rewritten command, not the original
/// one. So the key travels inside the command, and the post side needs no
/// other record of the original.
fn narrowed_bash_key(command: &str) -> Option<&str> {
    let prefix = format!("head -{} '", intercept::NARROWED_LINES);
    command.strip_prefix(prefix.as_str())?.strip_suffix('\'')
}

/// Builds the `PreToolUse` allow payload that rewrites the Bash command.
/// The payload keeps every other `tool_input` field, such as `description`.
fn pre_bash_narrow_json(input: &Value, key: &str) -> String {
    let mut tool_input = input
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    tool_input["command"] = Value::from(narrowed_bash_command(key));
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": tool_input,
        },
    })
    .to_string()
}

/// Answers a narrowed Bash read (F1). If the command is one
/// `handle_pre_bash` built, and `pre-read` recorded a note for its key,
/// the handler returns the `PostToolUse` JSON with the note and clears the
/// record. Otherwise it returns `None`.
fn handle_post_bash(input: &Value, project_dir: &Path) -> Option<String> {
    let command = input
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(Value::as_str)?;
    let key = narrowed_bash_key(command)?.to_string();
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let note = update_session(project_dir, session_id, |session| {
        session.narrowed.remove(&key)
    })?;
    context_json("PostToolUse", &note)
}

/// Builds the `PreToolUse` deny payload text for [`handle_pre_read`]'s
/// unchanged path, one line, no trailing newline.
fn pre_read_deny_json(file_path: &str) -> String {
    #[derive(Serialize)]
    struct HookOutput<'a> {
        #[serde(rename = "hookEventName")]
        event: &'a str,
        #[serde(rename = "permissionDecision")]
        decision: &'a str,
        #[serde(rename = "permissionDecisionReason")]
        reason: &'a str,
    }
    #[derive(Serialize)]
    struct DenyPayload<'a> {
        #[serde(rename = "hookSpecificOutput")]
        hook_specific_output: HookOutput<'a>,
    }
    let reason = &format!(
        "[sieve] {} is unchanged since your last read. Use that copy. \
         If you no longer hold it, read again.",
        basename(file_path)
    );
    let payload = DenyPayload {
        hook_specific_output: HookOutput {
            event: "PreToolUse",
            decision: "deny",
            reason,
        },
    };
    serde_json::to_string(&payload).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// tool-savings
// ---------------------------------------------------------------------------

fn handle_tool_use(input: &Value, project_dir: &Path) {
    let tool_name = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let command = input
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(Value::as_str);
    let (kind, saved) = classify_and_score(tool_name, command, || {
        input
            .get("tool_response")
            .cloned()
            .unwrap_or_else(|| input.clone())
    });
    let session_id = session_id_of(input);
    record_tool_use(project_dir, &session_id, kind, saved, "claude-code");
}

/// Classifies a tool use and, only when it could carry a footer, parses
/// the savings out of its lazily built payload (`classifyAndScore`). A
/// source read never prints a footer, so its payload is never built.
fn classify_and_score(
    tool_name: &str,
    command: Option<&str>,
    payload: impl FnOnce() -> Value,
) -> (Option<&'static str>, u64) {
    let mut kind = classify_tool_use(tool_name, command);
    if kind == Some("source") {
        return (kind, 0);
    }
    let blob = serde_json::to_string(&payload()).unwrap_or_default();
    let saved = sum_savings_footers(&blob);
    if saved > 0 {
        kind = Some("sieve");
    }
    (kind, saved)
}

/// The first of `keys` in `input` that is neither absent nor `null`
/// (a chain of `??`).
fn first_present<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .filter_map(|k| input.get(*k))
        .find(|v| !v.is_null())
}

/// Cursor keys a chat by `conversation_id`, else `session_id`
/// (`cursorSessionId`).
fn cursor_session_id(input: &Value) -> String {
    // A string or a non-zero number counts, as in a JS `||` chain. An
    // array, an object or a bool falls to the next key: a deviation, so an
    // odd value never names a file.
    ["conversation_id", "session_id"]
        .iter()
        .filter_map(|k| match input.get(*k) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) if n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()) => {
                Some(n.to_string())
            }
            _ => None,
        })
        .next()
        .unwrap_or_else(|| "default".to_string())
}

/// Whether a tool name is server-prefixed, as opposed to a native Read or
/// Shell (`isMcpToolName`).
fn is_mcp_tool_name(tool_name: &str) -> bool {
    let t = tool_name.to_lowercase();
    t.starts_with("mcp") || t.contains(':') || t.contains("__")
}

/// Cursor `postToolUse` (`handleCursorPostTool`). An MCP tool call is
/// skipped: `cursor-mcp` owns it, and counting both would double the tally.
fn handle_cursor_post_tool(input: &Value, project_dir: &Path) {
    let tool_name = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    if is_mcp_tool_name(tool_name) || is_sieve_mcp_tool(&tool_name.to_lowercase()) {
        return;
    }
    let tool_input = input.get("tool_input").unwrap_or(&Value::Null);
    let command = first_present(tool_input, &["command", "cmd"]).and_then(Value::as_str);
    let (kind, saved) = classify_and_score(tool_name, command, || {
        first_present(input, &["tool_output", "tool_response"])
            .cloned()
            .unwrap_or_else(|| input.clone())
    });
    record_tool_use(
        project_dir,
        &cursor_session_id(input),
        kind,
        saved,
        "cursor",
    );
}

/// Cursor `afterMCPExecution` (`handleCursorMcp`): counts a sieve MCP
/// call, with its savings read out of `result_json`.
fn handle_cursor_mcp(input: &Value, project_dir: &Path) {
    let tool_name = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    if !is_sieve_mcp_tool(&tool_name.to_lowercase()) {
        return;
    }
    let payload = first_present(input, &["result_json", "result"]).unwrap_or(input);
    let saved = sum_savings_footers(&serde_json::to_string(payload).unwrap_or_default());
    record_tool_use(
        project_dir,
        &cursor_session_id(input),
        Some("sieve"),
        saved,
        "cursor",
    );
}

/// Native tools that mean "the agent went to read source itself"
/// (`SOURCE_TOOLS`).
const SOURCE_TOOLS: [&str; 4] = ["read", "grep", "glob", "search"];

fn classify_tool_use(tool_name: &str, command: Option<&str>) -> Option<&'static str> {
    let t = tool_name.to_lowercase();
    if t.is_empty() {
        return None;
    }
    if is_sieve_mcp_tool(&t) {
        return Some("sieve");
    }
    if SOURCE_TOOLS.contains(&t.as_str()) {
        return Some("source");
    }
    if (t == "bash" || t == "shell") && command.is_some_and(command_invokes_sieve) {
        return Some("sieve");
    }
    None
}

/// Whether `tool_name` names one of the MCP tools, across a host
/// prefix (`mcp__sieve__sieve_find_code`, `MCP:sieve_find_code`) — the
/// last `[:./]`- or `__`-delimited segment, checked against the real name
/// list.
fn is_sieve_mcp_tool(tool_name_lower: &str) -> bool {
    let bare = tool_name_lower
        .split("__")
        .flat_map(|s| s.split([':', '.', '/']))
        .last()
        .unwrap_or(tool_name_lower);
    // Strips the active product's own `<name>_` prefix and matches the six
    // bare tool suffixes by hand, instead of calling
    // `sieve_daemon::names::tool_names()` (six `String` allocations) once
    // per hook tool-use event.
    const TOOL_BASES: [&str; 6] = [
        "find_code",
        "file_api",
        "check_freshness",
        "trace_calls",
        "find_all",
        "repo_map",
    ];
    let is_named_tool = bare
        .strip_prefix(product().name)
        .and_then(|rest| rest.strip_prefix('_'))
        .is_some_and(|base| TOOL_BASES.contains(&base));
    is_named_tool || sieve_daemon::names::canonical_tool_name(bare) != bare
}

/// Whether a shell command line invokes the sieve CLI, any install shape.
/// ponytail: a manual scan, not a regex (no `regex` crate here) — covers the
/// shapes the goldens and the session examples name; a wilder invocation
/// (`sieve-dev` behind three pipes) can still slip past.
fn command_invokes_sieve(command: &str) -> bool {
    let c = command.trim();
    for segment in c.split(['|', '&', ';']) {
        let mut rest = segment.trim_start();
        // Leading `NAME=value` words are an env prefix.
        while let Some(word) = rest.split_whitespace().next() {
            let is_assign = word.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            if !is_assign {
                break;
            }
            rest = rest[word.len()..].trim_start();
        }
        for prefix in ["npx -y ", "npx "] {
            if let Some(r) = rest.strip_prefix(prefix) {
                rest = r;
                break;
            }
        }
        // A path form such as `./target/release/sieve` counts by its last part.
        let first_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if let Some(slash) = rest[..first_end].rfind('/') {
            rest = &rest[slash + 1..];
        }
        for name in ["sieve-dev", "sieve"] {
            if let Some(after) = rest.strip_prefix(name) {
                let boundary = after
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'));
                if boundary {
                    return true;
                }
            }
        }
    }
    false
}

/// Sums every `[<product>] tokens saved \u{2248} N` footer in `text`
/// (`sumSavingsFooters`).
fn sum_savings_footers(text: &str) -> u64 {
    let name = product().name;
    // The product prints the short `[sieve] saved \u{2248} N tokens`
    // header. It still reads the long form, which an older transcript holds.
    let long = format!("[{name}] tokens saved \u{2248} ");
    sum_after_marker(text, &format!("[{name}] saved \u{2248} ")) + sum_after_marker(text, &long)
}

/// Sums the number that follows each `marker` in `text`.
fn sum_after_marker(text: &str, marker: &str) -> u64 {
    let mut total = 0u64;
    let mut rest = text;
    while let Some(idx) = rest.find(marker) {
        let after = &rest[idx + marker.len()..];
        let digits: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ',')
            .collect();
        if let Ok(n) = digits.replace(',', "").parse::<u64>() {
            total += n;
        }
        rest = &after[digits.len()..];
    }
    total
}

/// Folds one tool use into a session's counters (`recordToolUse`).
fn record_tool_use(
    project_dir: &Path,
    session_id: &str,
    kind: Option<&str>,
    saved: u64,
    host: &str,
) {
    if kind.is_none() && saved == 0 {
        return;
    }
    update_session(project_dir, session_id, |session| {
        match kind {
            Some("sieve") => session.tool_reads += 1,
            Some("source") => session.source_reads += 1,
            _ => {}
        }
        if saved > 0 {
            session.add_saved(saved);
        }
        if kind == Some("sieve") {
            session.turn_used_sieve = Some(true);
        }
        if !host.is_empty() && session.host.is_none() {
            session.host = Some(host.to_string());
        }
    });
}

// ---------------------------------------------------------------------------
// stop
// ---------------------------------------------------------------------------

fn handle_stop(input: &Value, project_dir: &Path) {
    count_tally_turn(input, project_dir);
    count_lost_notes(input, project_dir);
    // The sync gate: Sieve has nothing to spawn, so this always returns,
    // matching the golden `hook-stop`'s empty stdout.
    //
    // The billing sample of a turn needs a price table
    // Sieve does not have ("no billing"), and
    // every golden's transcript is unreadable anyway, so it is out of
    // scope here.
}

/// Adds the leftover F1 notes of this session to `Stats.notes_lost` and
/// clears them (F1). A leftover note means that `post-read` never ran for
/// a narrowed Read. The count is the note failure record the F1 design
/// asks for. The clear stops the session file from growing. A session with
/// no leftover note writes nothing.
fn count_lost_notes(input: &Value, project_dir: &Path) {
    let id = session_id_of(input);
    if read_session(project_dir, &id).narrowed.is_empty() {
        return;
    }
    let lost = update_session(project_dir, &id, |session| {
        std::mem::take(&mut session.narrowed).len() as u64
    });
    if lost > 0 {
        patch_stats(project_dir, |stats| stats.notes_lost += lost);
    }
}

/// One turn's assistant prose, plus the id of the entry it ended on
/// (`AssistantTurn`).
struct AssistantTurn {
    uuid: String,
    text: String,
}

/// Did this turn's reply tell the user what sieve saved
/// (`countTallyTurn`, `handleStop`'s Stop-time tally)?
fn count_tally_turn(input: &Value, project_dir: &Path) {
    let id = session_id_of(input);
    if !read_session(project_dir, &id)
        .turn_used_sieve
        .unwrap_or(false)
    {
        return;
    }
    let transcript_path = input.get("transcript_path").and_then(Value::as_str);
    let turn = transcript_path.and_then(last_assistant_turn);
    update_session(project_dir, &id, |session| match turn {
        Some(turn) if Some(turn.uuid.as_str()) != session.last_tally_uuid.as_deref() => {
            session.sieve_turns = Some(session.sieve_turns.unwrap_or(0) + 1);
            if has_savings_tally(&turn.text) {
                session.reported_turns = Some(session.reported_turns.unwrap_or(0) + 1);
            }
            session.turn_used_sieve = Some(false);
            session.last_tally_uuid = Some(turn.uuid);
        }
        _ => session.turn_used_sieve = Some(false),
    });
}

/// Reads the last `max_bytes` of `path`, dropping the leading partial
/// line a byte-offset read leaves behind (`readTail`). `None` on any I/O
/// trouble — a hook never fails a metric.
fn read_tail(path: &str, max_bytes: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let take = len.min(max_bytes);
    file.seek(SeekFrom::Start(len - take)).ok()?;
    let mut buf = vec![0u8; take as usize];
    file.read_exact(&mut buf).ok()?;
    let raw = String::from_utf8_lossy(&buf).into_owned();
    if take < len {
        match raw.find('\n') {
            Some(idx) => Some(raw[idx + 1..].to_string()),
            None => Some(raw),
        }
    } else {
        Some(raw)
    }
}

fn is_user_prompt(entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    if entry
        .get("isMeta")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    match entry.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(_)) => true,
        Some(Value::Array(parts)) => !parts
            .iter()
            .any(|p| p.get("type").and_then(Value::as_str) == Some("tool_result")),
        _ => false,
    }
}

fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .map(|p| p.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The assistant prose of the turn the transcript ends on, or `None` when
/// it can't be read (`lastAssistantTurn`).
fn last_assistant_turn(transcript_path: &str) -> Option<AssistantTurn> {
    if transcript_path.is_empty() {
        return None;
    }
    let tail = read_tail(transcript_path, TRANSCRIPT_TAIL_BYTES)?;
    if tail.is_empty() {
        return None;
    }
    let entries: Vec<Value> = tail
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let mut parts: Vec<String> = Vec::new();
    let mut uuid: Option<String> = None;
    for entry in entries.iter().rev() {
        if entry
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        if is_user_prompt(entry) {
            break;
        }
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let text = text_of(entry.get("message").and_then(|m| m.get("content")));
        if text.trim().is_empty() {
            continue;
        }
        if uuid.is_none() {
            uuid = entry
                .get("uuid")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        parts.insert(0, text);
    }
    let uuid = uuid?;
    if parts.is_empty() {
        return None;
    }
    Some(AssistantTurn {
        uuid,
        text: parts.join("\n"),
    })
}

/// Whether `c` is a JS regex `\s` character.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn skip_spaces(s: &[char]) -> &[char] {
    let n = s.iter().take_while(|c| is_js_space(**c)).count();
    &s[n..]
}

/// Whether `s` starts with a tally: `<name>\s+saved\s*[~\u{2248}]?\s*[\d,.]+\s*[km]?\s*tok`
/// on ASCII-lowercased input. Every class is disjoint from the one after
/// it, so a greedy scan with no backtracking finds the same matches.
fn tally_at(s: &[char], name: &[char]) -> bool {
    let Some(s) = s.strip_prefix(name) else {
        return false;
    };
    let after_name = skip_spaces(s);
    if after_name.len() == s.len() {
        return false;
    }
    let Some(s) = after_name.strip_prefix(&['s', 'a', 'v', 'e', 'd']) else {
        return false;
    };
    let s = skip_spaces(s);
    let s = s
        .strip_prefix(&['~'])
        .or(s.strip_prefix(&['\u{2248}']))
        .unwrap_or(s);
    let s = skip_spaces(s);
    let numerals = s
        .iter()
        .take_while(|c| c.is_ascii_digit() || **c == ',' || **c == '.')
        .count();
    if numerals == 0 {
        return false;
    }
    let s = skip_spaces(&s[numerals..]);
    let s = s
        .strip_prefix(&['k'])
        .or(s.strip_prefix(&['m']))
        .unwrap_or(s);
    skip_spaces(s).starts_with(&['t', 'o', 'k'])
}

/// Whether `text` tells the user what the product saved, per the `TALLY`
/// regex:
/// `/sieve\s+saved\s*[~\u{2248}]?\s*[\d,.]+\s*[km]?\s*(?:tok|tokens)/i`.
/// The word `sieve` is the active product name. The scan is a hand-written
/// matcher because this crate has no `regex` dependency.
fn has_savings_tally(text: &str) -> bool {
    has_tally_for(text, product().name)
}

/// [`has_savings_tally`] for an explicit product `name` in lower case.
fn has_tally_for(text: &str, name: &str) -> bool {
    let chars: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    let name: Vec<char> = name.chars().collect();
    (0..chars.len()).any(|i| tally_at(&chars[i..], &name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p4_24_session_rewrite_keeps_the_billing_fields() {
        let raw = r#"{"savedTokens":5,"inputCostMicros":5000000,"inputTokensBilled":1000000}"#;
        let state: SessionState = serde_json::from_str(raw).unwrap();
        let again = serde_json::to_string(&state).unwrap();
        assert!(again.contains("\"inputCostMicros\":5000000"), "{again}");
        assert!(again.contains("\"inputTokensBilled\":1000000"), "{again}");
        let bare = serde_json::to_string(&SessionState::default()).unwrap();
        assert!(!bare.contains("inputCostMicros"), "{bare}");
    }

    #[test]
    fn utf16_prefix_counts_units_not_bytes_or_chars() {
        // An em dash is 1 unit, 3 bytes; an emoji is 2 units, 1 char.
        assert_eq!(utf16_prefix("a\u{2014}bc", 3), "a\u{2014}b");
        assert_eq!(utf16_prefix("\u{1F600}\u{1F600}x", 4), "\u{1F600}\u{1F600}");
        // A cut inside a pair drops the whole pair.
        assert_eq!(utf16_prefix("\u{1F600}\u{1F600}x", 3), "\u{1F600}");
        assert_eq!(utf16_prefix("short", 100), "short");
    }
    use sieve_core::wiring::{Confidence, Kind, Meta, Origin, Relation, SummaryState};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A temp dir that removes itself on drop (mirrors the pattern in
    /// `hosts.rs`'s test module).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("sieve-hook-{label}-{pid}-{n}-{nanos}"));
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn file_node(path: &str) -> Node {
        Node {
            id: path.to_string(),
            name: basename(path),
            kind: Kind::File,
            path: path.to_string(),
            span: "L1-L1".to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: String::new(),
            chars: None,
            body_text: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
            arity: None,
            variadic: None,
        }
    }

    fn symbol_node(id: &str, name: &str, path: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind: Kind::Function,
            path: path.to_string(),
            span: "L1-L2".to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: String::new(),
            chars: None,
            body_text: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
            arity: None,
            variadic: None,
        }
    }

    fn edge(source: &str, target: &str, relation: Relation) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            relation,
            confidence: Confidence::Extracted,
        }
    }

    #[test]
    fn index_freshness_counts_missing_files_and_banner_fires() {
        let dir = TempDir::new("freshness-missing");
        let project_dir = dir.path.clone();
        let context_dir = project_dir.join("sieve");
        std::fs::create_dir_all(&context_dir).expect("create context dir");
        std::fs::write(project_dir.join("present.ts"), "x").expect("write present file");
        let manifest = r#"{"version":1,"model":"m","repoDigest":"d","files":[
            {"path":"present.ts","hash":"h1"},
            {"path":"missing.ts","hash":"h2"}
        ],"nodes":[]}"#;
        std::fs::write(context_dir.join("manifest.json"), manifest).expect("write manifest");

        let freshness = index_freshness(&project_dir, &context_dir).expect("has manifest");
        assert_eq!(freshness.missing, 1);
        assert_eq!(freshness.total, 2);
        assert_eq!(
            stale_banner(Some(&freshness)),
            Some(
                "\u{26a0} sieve's index may be ahead of your working tree: 1 of 2 indexed \
                files are not on disk (branch switch or uncommitted move?). If sieve names a path that \
                isn't there, don't chase it \u{2014} `sieve grep` the symbol to find where it lives now; \
                run `sieve build` to refresh."
                    .to_string()
            )
        );
    }

    #[test]
    fn index_freshness_stays_silent_when_every_file_is_present() {
        let dir = TempDir::new("freshness-present");
        let project_dir = dir.path.clone();
        let context_dir = project_dir.join("sieve");
        std::fs::create_dir_all(&context_dir).expect("create context dir");
        std::fs::write(project_dir.join("present.ts"), "x").expect("write present file");
        let manifest = r#"{"version":1,"model":"m","repoDigest":"d","files":[
            {"path":"present.ts","hash":"h1"}
        ],"nodes":[]}"#;
        std::fs::write(context_dir.join("manifest.json"), manifest).expect("write manifest");

        let freshness = index_freshness(&project_dir, &context_dir).expect("has manifest");
        assert_eq!(freshness.missing, 0);
        assert_eq!(stale_banner(Some(&freshness)), None);
    }

    #[test]
    fn index_freshness_is_none_without_a_manifest() {
        let dir = TempDir::new("freshness-no-manifest");
        let project_dir = dir.path.clone();
        let context_dir = project_dir.join("sieve");
        std::fs::create_dir_all(&context_dir).expect("create context dir");

        assert!(index_freshness(&project_dir, &context_dir).is_none());
        assert_eq!(stale_banner(None), None);
    }

    #[test]
    fn under_sieve_matches_the_context_dir() {
        assert!(under_sieve(Path::new("/repo"), "/repo/sieve/INDEX.md"));
        assert!(!under_sieve(Path::new("/repo"), "/repo/src/util.ts"));
    }

    #[test]
    fn command_invokes_sieve_matches_the_cli_and_npx_shapes() {
        assert!(command_invokes_sieve("sieve ask \"x\" . --json -n 3"));
        assert!(!command_invokes_sieve("mysieve build"));
        assert!(!command_invokes_sieve("echo not-a-sieve-call"));
    }

    #[test]
    fn command_invokes_sieve_matches_a_sieve_command_in_every_form() {
        for yes in [
            "sieve ask \"x\" . --json",
            "./target/release/sieve map",
            "/usr/local/bin/sieve grep foo",
            "npx sieve ask x",
            "npx -y sieve ask x",
            "SIEVE_DIR=x sieve callers run",
            "A=1 B=2 ./bin/sieve skeleton f.ts",
            "cd repo && sieve map",
            "sieve-dev map",
        ] {
            assert!(command_invokes_sieve(yes), "{yes}");
        }
        for no in [
            "mysieve build",
            "sieve-mytool run",
            "echo sieve",
            "cat src/sieve.ts",
            "FOO=sieve ls",
            "ls ./sieve-tools/x",
        ] {
            assert!(!command_invokes_sieve(no), "{no}");
        }
    }

    #[test]
    fn sum_savings_footers_adds_every_marker() {
        let text =
            "[sieve] tokens saved \u{2248} 420 (72%)... [sieve] tokens saved \u{2248} 1,200 more";
        assert_eq!(sum_savings_footers(text), 1620);
    }

    #[test]
    fn test_savings_short_header_is_read_by_the_marker() {
        let text = "[sieve] saved \u{2248} 1,200 tokens\n\n... [sieve] saved \u{2248} 30 tokens";
        assert_eq!(sum_after_marker(text, "[sieve] saved \u{2248} "), 1230);
        assert_eq!(sum_after_marker(text, "[sieve] tokens saved \u{2248} "), 0);
    }

    #[test]
    fn weak_match_nudge_stops_after_the_cap() {
        let mut session = SessionState::default();
        assert!(weak_match_nudge(&mut session, "how does run work").is_some());
        assert!(weak_match_nudge(&mut session, "how does run work").is_some());
        assert!(weak_match_nudge(&mut session, "how does run work").is_none());
        assert_eq!(session.nudges, 2);
    }

    #[test]
    fn format_blast_radius_matches_the_post_edit_golden() {
        let app = file_node("src/app.ts");
        let util = file_node("src/util.ts");
        let run = symbol_node("app.ts#run", "run", "src/app.ts");
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 3,
                edge_count: 2,
                languages: vec!["ts".to_string()],
                scopes: Vec::new(),
            },
            nodes: vec![app.clone(), util, run.clone()],
            edges: vec![
                edge("src/app.ts", "src/util.ts", Relation::Imports),
                edge("app.ts#run", "src/util.ts", Relation::Calls),
            ],
        };
        let text = format_blast_radius(&graph, "/tmp/copy/src/util.ts", 8).unwrap();
        assert_eq!(
            text,
            "[sieve] blast radius for util.ts, who depends on it:\n \u{2022} imports \u{2190} app.ts (app.ts)\n \u{2022} calls \u{2190} run (app.ts)"
        );
    }

    /// The `has_savings_tally` verdicts for each text, read by running the real
    /// function.
    #[test]
    fn test_p4_55_tally_matches_golden_regex() {
        let cases: [(&str, bool); 22] = [
            ("\u{1f331} sieve saved ~12k tokens this turn, 3 calls", true),
            ("SIEVE  SAVED\u{2248} 1,234.5 K TOK", true),
            ("sieve saved 12", false),
            ("sieve saved ~ tokens", false),
            ("sieve saved12k tok", true),
            ("sievesaved 12 tok", false),
            ("sieve is great software", false),
            ("sieve saved ~12k toks", true),
            ("sieve saved 5 mtok", true),
            ("sieve saved ~12k\ntokens", true),
            ("mysieve saved 3 tok", true),
            ("sieve saved ~-3 tok", false),
            ("sieve saved .. tok", true),
            ("sieve saved nothing, tokens are cheap", false),
            ("sieve saved \u{2248}~5 tok", false),
            ("sieve\u{a0}saved\u{a0}5\u{a0}tok", true),
            ("sieve saved 5 ktok", true),
            ("sieve saved 5 kmtok", false),
            ("sieve saved 5 k tok", true),
            ("sieve saved 5 t", false),
            ("xx sieve saved 5 tok", true),
            ("sieve saved ~12k (~$0.04) tokens", false),
        ];
        for (text, want) in cases {
            assert_eq!(has_savings_tally(text), want, "{text:?}");
        }
    }

    /// Every odd id must write only a direct child of the session dir.
    #[test]
    fn test_p4_54_session_id_never_leaves_the_session_dir_in_session_path() {
        let dir = TempDir::new("session-id-escape");
        let project = dir.path.join("project");
        let session_dir = cache_dir(&project).join("session");
        let long = "x".repeat(1000);
        let ids = [
            "../../../../ESC",
            "a/b/c",
            "..",
            ".",
            "/tmp/x",
            "a\\b\\..\\c",
            "nul\0byte",
            "",
            long.as_str(),
            "\u{e9}\u{1f600}",
        ];
        for id in ids {
            update_session(&project, id, |s| s.nudges = 7);
            let path = session_path(&project, id);
            assert_eq!(path.parent(), Some(session_dir.as_path()), "{id:?}");
            assert!(path.is_file(), "{id:?}: not written at {path:?}");
            let name = path.file_name().expect("name").to_string_lossy();
            assert!(name.len() <= SESSION_ID_MAX + ".json".len(), "{id:?}");
        }
        // Nothing but the project dir exists beside it.
        let beside: Vec<_> = std::fs::read_dir(&dir.path).expect("read").collect();
        assert_eq!(beside.len(), 1);
        fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).expect("read dir").flatten() {
                let p = e.path();
                if p.is_dir() {
                    files_under(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        files_under(&project, &mut files);
        assert!(
            files.iter().all(|f| f.starts_with(&session_dir)),
            "{files:?}"
        );
    }

    #[test]
    fn test_p4_54_session_id_never_leaves_the_session_dir_names() {
        let uuid = "3f2b8c1e-9d4a-4f6b-8a7c-1e2d3c4b5a69";
        assert_eq!(safe_session_stem(uuid), uuid);
        assert_eq!(safe_session_stem("default"), "default");
        assert_eq!(safe_session_stem("a.b_c-d"), "a.b_c-d");
        assert_eq!(safe_session_stem("a/b/c"), "a_b_c");
        assert_eq!(safe_session_stem("../../../../ESC"), "_.._.._.._.._ESC");
        assert_eq!(safe_session_stem(".."), "_..");
        assert_eq!(safe_session_stem("."), "_.");
        assert_eq!(safe_session_stem(""), "_");
        assert_eq!(safe_session_stem("/tmp/x"), "_tmp_x");
        assert_eq!(safe_session_stem("a\\b"), "a_b");
        assert_eq!(safe_session_stem("a\0b"), "a_b");
        assert_eq!(safe_session_stem(&"x".repeat(1000)), "x".repeat(128));
        assert_eq!(safe_session_stem(&"y".repeat(128)), "y".repeat(128));
    }

    /// Under the sieve product the regex word is `sieve`: the orientation
    /// text brands its tally example `sieve saved ~12k tokens`.
    #[test]
    fn test_p4_55_tally_word_is_the_product_name() {
        assert!(has_tally_for("sieve saved ~12k tokens this turn", "sieve"));
        assert!(!has_tally_for("other saved ~12k tokens this turn", "sieve"));
        assert!(!has_tally_for("sieve saved ~12k tokens this turn", "other"));
    }

    /// The `PreToolUse` stdin shape `handle_pre_read` reads: a session id
    /// and `tool_input.file_path`.
    fn pre_read_input(session_id: &str, file_path: &Path) -> Value {
        serde_json::json!({
            "session_id": session_id,
            "tool_input": { "file_path": file_path.to_string_lossy() },
        })
    }

    #[test]
    fn test_f3_pre_read_denies_an_unchanged_file() {
        let dir = TempDir::new("pre-read-denies-unchanged");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "one").expect("write file");
        let input = pre_read_input("s1", &file);

        assert_eq!(handle_pre_read(&input, &dir.path), None);
        let second = handle_pre_read(&input, &dir.path);
        assert!(
            second.is_some(),
            "the second read of an unchanged file must deny"
        );
        assert!(second.unwrap().contains("\"permissionDecision\":\"deny\""));
    }

    #[test]
    fn test_f3_deny_text_names_the_escape() {
        let dir = TempDir::new("pre-read-deny-names-escape");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "one").expect("write file");
        let input = pre_read_input("s1", &file);

        assert_eq!(handle_pre_read(&input, &dir.path), None);
        let second = handle_pre_read(&input, &dir.path).expect("second read must deny");
        assert!(
            second.contains("again"),
            "the deny text must name the escape: read again"
        );
    }

    #[test]
    fn test_f3_pre_read_allows_a_changed_file() {
        let dir = TempDir::new("pre-read-allows-changed");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "one").expect("write file");
        let input = pre_read_input("s1", &file);

        assert_eq!(handle_pre_read(&input, &dir.path), None);
        std::fs::write(&file, "two, new bytes").expect("rewrite file");
        assert_eq!(handle_pre_read(&input, &dir.path), None);
    }

    #[test]
    fn test_f3_pre_read_allows_the_second_denied_read() {
        let dir = TempDir::new("pre-read-one-escape");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "one").expect("write file");
        let input = pre_read_input("s1", &file);

        assert_eq!(
            handle_pre_read(&input, &dir.path),
            None,
            "first read passes"
        );
        assert!(
            handle_pre_read(&input, &dir.path).is_some(),
            "second read denies"
        );
        assert_eq!(
            handle_pre_read(&input, &dir.path),
            None,
            "third read spends the one escape and passes"
        );
    }

    #[test]
    fn test_f3_pre_read_adds_to_saved_tokens() {
        let dir = TempDir::new("pre-read-saved-tokens");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "some bytes to read twice").expect("write file");
        let input = pre_read_input("s1", &file);

        handle_pre_read(&input, &dir.path);
        handle_pre_read(&input, &dir.path);

        let session = read_session(&dir.path, "s1");
        assert!(session.saved_tokens > 0);
    }

    #[test]
    fn test_f3_pre_read_ignores_a_missing_session_id() {
        let dir = TempDir::new("pre-read-no-session-id");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "one").expect("write file");
        let input = serde_json::json!({
            "tool_input": { "file_path": file.to_string_lossy() },
        });

        assert_eq!(
            handle_pre_read(&input, &dir.path),
            None,
            "first read with no session id must print nothing"
        );
        assert_eq!(
            handle_pre_read(&input, &dir.path),
            None,
            "a second read with no session id must never deny — two such \
             sessions must never share one record"
        );
    }

    #[test]
    fn test_f3_pre_read_matches_a_relative_and_an_absolute_path() {
        let dir = TempDir::new("pre-read-canonical-path");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "one").expect("write file");
        let canonical_dir = std::fs::canonicalize(&dir.path).expect("canonicalize temp dir");
        let absolute_input = pre_read_input("s1", &canonical_dir.join("a.txt"));
        let relative_input = pre_read_input("s1", Path::new("a.txt"));

        assert_eq!(
            handle_pre_read(&absolute_input, &dir.path),
            None,
            "first read, by the absolute path, must pass"
        );

        let previous_dir = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(&canonical_dir).expect("set current dir to temp dir");
        let second = handle_pre_read(&relative_input, &dir.path);
        std::env::set_current_dir(previous_dir).expect("restore current dir");

        assert!(
            second.is_some(),
            "a relative-path read of the same unchanged file must deny"
        );
    }

    #[test]
    fn test_f3_two_writers_do_not_lose_the_denied_flag() {
        let dir = TempDir::new("two-writers");
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                // The sleep holds the lock, so the two calls must overlap.
                // Do not delete it: without it the test can pass with no lock.
                update_session(&dir.path, "s1", |s| {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    s.reads.insert(
                        "a".into(),
                        ReadRecord {
                            hash: "h".into(),
                            denied: true,
                        },
                    );
                });
            });
            scope.spawn(|| {
                barrier.wait();
                update_session(&dir.path, "s1", |s| {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    s.saved_tokens += 7;
                });
            });
        });
        let session = read_session(&dir.path, "s1");
        assert!(session.reads.get("a").is_some_and(|r| r.denied));
        assert_eq!(session.saved_tokens, 7);
    }

    #[test]
    fn test_f3_two_writers_do_not_lose_a_stats_update() {
        let dir = TempDir::new("two-stats-writers");
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                // The sleep holds the lock, so the two calls must overlap.
                // Do not delete it: without it the test can pass with no lock.
                patch_stats(&dir.path, |s| {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    s.node_count += 3;
                });
            });
            scope.spawn(|| {
                barrier.wait();
                patch_stats(&dir.path, |s| {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    s.edge_count += 4;
                });
            });
        });
        let stats = read_stats(&dir.path).expect("stats file");
        assert_eq!(stats.node_count, 3);
        assert_eq!(stats.edge_count, 4);
    }

    #[test]
    fn test_f3_update_session_works_when_the_lock_is_held() {
        let dir = TempDir::new("lock-held");
        let lock_dir = session_path(&dir.path, "s1")
            .parent()
            .map(Path::to_path_buf);
        let lock_dir = lock_dir.expect("session dir");
        let _held = sieve_core::lock::LockGuard::acquire(&lock_dir)
            .expect("acquire lock")
            .expect("lock is free");

        let start = std::time::Instant::now();
        update_session(&dir.path, "s1", |s| s.saved_tokens = 5);

        assert!(start.elapsed() < std::time::Duration::from_millis(1000));
        assert_eq!(read_session(&dir.path, "s1").saved_tokens, 5);
    }

    // -----------------------------------------------------------------------
    // F1, "narrow a large Read"
    // -----------------------------------------------------------------------

    /// The `PreToolUse` stdin shape for a Read tool call: `pre_read_input`
    /// plus the `tool_name` that `classify` needs.
    fn read_tool_input(session_id: &str, file_path: &Path) -> Value {
        let mut input = pre_read_input(session_id, file_path);
        input["tool_name"] = Value::from("Read");
        input
    }

    /// A project with one large Rust file, and its graph cached under the
    /// context dir. The file is many times larger than its skeleton, and
    /// larger than `intercept::MIN_RAW_BYTES`. It is smaller than
    /// `intercept::BASH_OUTPUT_CAP`.
    fn large_file_project(label: &str) -> (TempDir, PathBuf) {
        large_file_project_with(label, 200)
    }

    /// [`large_file_project`] with `count` functions in the file.
    fn large_file_project_with(label: &str, count: u32) -> (TempDir, PathBuf) {
        let dir = TempDir::new(label);
        let mut source = String::new();
        for i in 0..count {
            source.push_str(&format!(
                "pub fn f{i}() -> u64 {{\n    let a = {i};\n    let b = a * 2;\n    let c = b + 1;\n    \
                 let d = c * c;\n    d + a\n}}\n\n"
            ));
        }
        let file = dir.path.join("src").join("big.rs");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, source).expect("write file");
        let ctx = resolve_context_dir(&dir.path);
        let graph = sieve_parse::build_graph(&dir.path, &ctx).expect("build graph");
        sieve_core::write_graph(&graph, &ctx.join(".graph").join("wiring.json")).expect("write");
        (dir, file)
    }

    #[test]
    fn test_f1_pre_read_narrows_a_large_file() {
        let (dir, file) = large_file_project("f1-narrows");
        // The stdin seam feeds the handler the same JSON the host sends.
        let seam = product().env_var("TEST_STDIN");
        std::env::set_var(&seam, read_tool_input("s1", &file).to_string());
        let input = read_stdin_json();
        std::env::remove_var(&seam);

        let out = handle_pre_read(&input, &dir.path).expect("a large file narrows");
        let json: Value = serde_json::from_str(&out).expect("valid JSON");
        let hso = &json["hookSpecificOutput"];
        assert_eq!(hso["hookEventName"], "PreToolUse");
        assert_eq!(hso["permissionDecision"], "allow");
        assert_eq!(hso["updatedInput"]["offset"], 1);
        assert_eq!(hso["updatedInput"]["limit"], intercept::NARROWED_LINES);
        assert_eq!(
            hso["updatedInput"]["file_path"],
            file.to_string_lossy().as_ref()
        );
        let key = std::fs::canonicalize(&file).expect("canonical");
        let session = read_session(&dir.path, "s1");
        assert!(
            session
                .narrowed
                .contains_key(key.to_string_lossy().as_ref()),
            "the narrowing must be recorded before the allow is printed"
        );
    }

    /// A Read input that names `offset` and `limit`.
    fn span_read_input(session_id: &str, file: &Path) -> Value {
        let mut input = read_tool_input(session_id, file);
        input["tool_input"]["offset"] = Value::from(6);
        input["tool_input"]["limit"] = Value::from(195);
        input
    }

    #[test]
    fn test_f1_pre_read_span_above_the_floor_passes_through() {
        let (dir, file) = long_line_project("f1-read-span");
        let input = span_read_input("s1", &file);
        let intercept::Intent::ReadFile {
            raw_bytes, bounded, ..
        } = intercept::classify(&input)
        else {
            panic!("a Read is a read");
        };
        assert!(bounded);
        assert_eq!(raw_bytes, 24_570);
        assert!(raw_bytes > intercept::MIN_RAW_BYTES);
        assert_eq!(handle_pre_read(&input, &dir.path), None);
        assert!(read_session(&dir.path, "s1").narrowed.is_empty());
    }

    #[test]
    fn test_f1_pre_read_plain_read_of_a_long_line_file_still_narrows() {
        let (dir, file) = long_line_project("f1-read-plain-long");
        let input = read_tool_input("s1", &file);
        assert!(handle_pre_read(&input, &dir.path).is_some());
    }

    #[test]
    fn test_f1_pre_read_span_passthrough_keeps_the_f3_deny() {
        let (dir, file) = long_line_project("f1-read-span-f3");
        let input = span_read_input("s1", &file);
        assert_eq!(handle_pre_read(&input, &dir.path), None);
        let second = handle_pre_read(&input, &dir.path).expect("F3 denies the repeat");
        assert!(second.contains("\"permissionDecision\":\"deny\""));
    }

    #[test]
    fn test_f1_pre_read_passes_a_small_file() {
        let (dir, _) = large_file_project("f1-small");
        let file = dir.path.join("src").join("small.rs");
        std::fs::write(&file, "pub fn one() {}\n").expect("write file");
        let ctx = resolve_context_dir(&dir.path);
        let graph = sieve_parse::build_graph(&dir.path, &ctx).expect("build graph");
        sieve_core::write_graph(&graph, &ctx.join(".graph").join("wiring.json")).expect("write");

        let input = read_tool_input("s1", &file);
        assert_eq!(handle_pre_read(&input, &dir.path), None);
        assert!(read_session(&dir.path, "s1").narrowed.is_empty());
    }

    #[test]
    fn test_f1_post_read_emits_the_note() {
        let (dir, file) = large_file_project("f1-post-note");
        let input = read_tool_input("s1", &file);
        handle_pre_read(&input, &dir.path).expect("narrows");

        let out = handle_post_read(&input, &dir.path).expect("the note follows a narrowing");
        let json: Value = serde_json::from_str(&out).expect("valid JSON");
        let hso = &json["hookSpecificOutput"];
        assert_eq!(hso["hookEventName"], "PostToolUse");
        let note = hso["additionalContext"].as_str().expect("note text");
        assert!(
            note.contains("fn f0"),
            "the note holds the skeleton: {note}"
        );
        assert!(
            note.contains("F1 narrowed this Read"),
            "the note names the narrowing: {note}"
        );
        assert_eq!(
            handle_post_read(&input, &dir.path),
            None,
            "the record clears after one note"
        );
    }

    #[test]
    fn test_f1_post_read_is_silent_with_no_record() {
        let dir = TempDir::new("f1-post-silent");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, "one").expect("write file");
        let input = read_tool_input("s1", &file);
        assert_eq!(handle_pre_read(&input, &dir.path), None);
        assert_eq!(handle_post_read(&input, &dir.path), None);
    }

    #[test]
    fn test_f1_a_failed_record_passes_the_read_through() {
        let (dir, file) = large_file_project("f1-record-fails");
        // A regular file where the session directory belongs makes every
        // session write fail. `write_json_atomic` cannot create the dir.
        let session_dir = cache_dir(&dir.path).join("session");
        std::fs::create_dir_all(session_dir.parent().expect("parent")).expect("mkdir");
        std::fs::write(&session_dir, "not a directory").expect("block the dir");

        let input = read_tool_input("s1", &file);
        assert_eq!(
            handle_pre_read(&input, &dir.path),
            None,
            "a narrowing with no record must pass the read through"
        );
        assert_eq!(handle_post_read(&input, &dir.path), None);
    }

    #[test]
    fn test_f1_a_narrowed_read_does_not_become_an_f3_deny() {
        let (dir, file) = large_file_project("f1-narrow-twice");
        let input = read_tool_input("s1", &file);
        let first = handle_pre_read(&input, &dir.path).expect("first read narrows");
        assert!(first.contains("updatedInput"));

        let second = handle_pre_read(&input, &dir.path).expect("second read narrows again");
        assert!(
            second.contains("updatedInput"),
            "F3 must not claim a copy F1 never delivered: {second}"
        );
        assert!(!second.contains("\"deny\""), "{second}");
    }

    #[test]
    fn test_f1_f3_denies_when_f1_passes_through() {
        let (dir, _) = large_file_project("f1-f3-small-deny");
        let file = dir.path.join("src").join("small.rs");
        std::fs::write(&file, "pub fn one() {}\n").expect("write file");
        let input = read_tool_input("s1", &file);

        assert_eq!(
            handle_pre_read(&input, &dir.path),
            None,
            "first read passes"
        );
        let second = handle_pre_read(&input, &dir.path).expect("second read denies");
        assert!(
            second.contains("\"permissionDecision\":\"deny\""),
            "F3 keeps its deny on a file F1 did not narrow: {second}"
        );
    }

    #[test]
    fn test_f1_post_read_is_silent_after_the_file_changes() {
        let (dir, file) = large_file_project("f1-stale-note");
        let input = read_tool_input("s1", &file);
        handle_pre_read(&input, &dir.path).expect("narrows");

        // The file shrinks. F1 now passes the read through in full.
        std::fs::write(&file, "pub fn f0() -> u64 {\n    0\n}\n").expect("rewrite file");
        assert_eq!(handle_pre_read(&input, &dir.path), None);
        assert_eq!(
            handle_post_read(&input, &dir.path),
            None,
            "a full read must not get the old note"
        );
    }

    // -----------------------------------------------------------------------
    // F1, the Bash read path
    // -----------------------------------------------------------------------

    /// The `PreToolUse` stdin shape for a Bash tool call.
    fn bash_tool_input(session_id: &str, command: &str) -> Value {
        serde_json::json!({
            "session_id": session_id,
            "tool_name": "Bash",
            "tool_input": { "command": command, "description": "read it" },
        })
    }

    fn canonical_key(file: &Path) -> String {
        std::fs::canonicalize(file)
            .expect("canonical")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn test_f1_pre_bash_cat_between_floor_and_cap_rewrites_to_head() {
        let (dir, file) = large_file_project("f1-bash-cat");
        let size = std::fs::metadata(&file).expect("metadata").len();
        assert!((intercept::MIN_RAW_BYTES..=intercept::BASH_OUTPUT_CAP).contains(&size));
        // The stdin seam feeds the handler the same JSON the host sends.
        let seam = product().env_var("TEST_STDIN");
        std::env::set_var(
            &seam,
            bash_tool_input("s1", &format!("cat {}", file.display())).to_string(),
        );
        let input = read_stdin_json();
        std::env::remove_var(&seam);

        let out = handle_pre_bash(&input, &dir.path).expect("a large cat narrows");
        let json: Value = serde_json::from_str(&out).expect("valid JSON");
        let hso = &json["hookSpecificOutput"];
        assert_eq!(hso["hookEventName"], "PreToolUse");
        assert_eq!(hso["permissionDecision"], "allow");
        let key = canonical_key(&file);
        assert_eq!(
            hso["updatedInput"]["command"],
            format!("head -{} '{key}'", intercept::NARROWED_LINES)
        );
        assert_eq!(hso["updatedInput"]["description"], "read it");
        assert!(
            read_session(&dir.path, "s1").narrowed.contains_key(&key),
            "the narrowing must be recorded before the allow is printed"
        );
    }

    #[test]
    fn test_f1_pre_bash_cat_above_the_harness_cap_passes_through() {
        let (dir, file) = large_file_project_with("f1-bash-big-cat", 400);
        let size = std::fs::metadata(&file).expect("metadata").len();
        assert!(size > intercept::BASH_OUTPUT_CAP, "{size}");
        let input = bash_tool_input("s1", &format!("cat {}", file.display()));
        assert_eq!(
            handle_pre_bash(&input, &dir.path),
            None,
            "the capped preview costs less than the skeleton"
        );
        assert!(read_session(&dir.path, "s1").narrowed.is_empty());
    }

    #[test]
    fn test_f1_pre_bash_cat_below_the_floor_passes_through() {
        let (dir, _) = large_file_project_with("f1-bash-small-cat", 20);
        let file = dir.path.join("src").join("big.rs");
        let size = std::fs::metadata(&file).expect("metadata").len();
        assert!(size < intercept::MIN_RAW_BYTES, "{size}");
        let input = bash_tool_input("s1", &format!("cat {}", file.display()));
        assert_eq!(handle_pre_bash(&input, &dir.path), None);
        assert!(read_session(&dir.path, "s1").narrowed.is_empty());
    }

    /// A project with one Rust file of 208 lines, each padded to 125
    /// columns with a trailing comment. The 2026-09-30 probe file had this
    /// shape. The span of lines 6 to 200 is 195 lines of 126 bytes, which
    /// is 24,570 bytes, above `intercept::MIN_RAW_BYTES`.
    fn long_line_project(label: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new(label);
        let mut source = String::new();
        for i in 0..26 {
            let lines = [
                format!("pub fn f{i}() -> u64 {{"),
                format!("    let a = {i};"),
                "    let b = a * 2;".to_string(),
                "    let c = b + 1;".to_string(),
                "    let d = c * c;".to_string(),
                "    d + a".to_string(),
                "}".to_string(),
                String::new(),
            ];
            for line in lines {
                let commented = format!("{line} //");
                source.push_str(&format!("{commented:<125}\n"));
            }
        }
        let file = dir.path.join("src").join("long.rs");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, source).expect("write file");
        let ctx = resolve_context_dir(&dir.path);
        let graph = sieve_parse::build_graph(&dir.path, &ctx).expect("build graph");
        sieve_core::write_graph(&graph, &ctx.join(".graph").join("wiring.json")).expect("write");
        (dir, file)
    }

    /// Asserts that a bounded command passes through, and that only the
    /// bound stops the rewrite. The ratio gate would fire on the cost.
    fn assert_bounded_passes(dir: &TempDir, command: &str) {
        let input = bash_tool_input("s1", command);
        let intent = intercept::classify(&input);
        let intercept::Intent::ReadFile {
            raw_bytes, bounded, ..
        } = &intent
        else {
            panic!("{command} is a read");
        };
        assert!(bounded, "{command} names a bound");
        assert_eq!(handle_pre_bash(&input, &dir.path), None, "{command}");
        assert!(read_session(&dir.path, "s1").narrowed.is_empty());
        // The cost alone would not have saved the command.
        let ctx = resolve_context_dir(&dir.path);
        let answer = intercept::answer_bash(&dir.path, &ctx, &intent);
        assert!(
            intercept::decide(&intent, Some(*raw_bytes), answer).is_some(),
            "{command} costs {raw_bytes} bytes, and the ratio gate fires on that"
        );
    }

    #[test]
    fn test_f1_pre_bash_sed_span_above_the_floor_passes_through() {
        let (dir, file) = long_line_project("f1-bash-sed-span");
        let size = std::fs::metadata(&file).expect("metadata").len();
        assert_eq!(size, 208 * 126, "{size}");
        let command = format!("sed -n '6,200p' {}", file.display());
        let intercept::Intent::ReadFile { raw_bytes, .. } =
            intercept::classify(&bash_tool_input("s1", &command))
        else {
            panic!("a sed span is a read");
        };
        assert_eq!(raw_bytes, 24_570);
        assert_bounded_passes(&dir, &command);
    }

    #[test]
    fn test_f1_pre_bash_head_count_passes_through() {
        let (dir, file) = long_line_project("f1-bash-head-count");
        // 5 lines cost 630 bytes. That is under the floor, so the pass
        // through needs no bound. The larger count proves the bound.
        let input = bash_tool_input("s1", &format!("head -5 {}", file.display()));
        assert_eq!(handle_pre_bash(&input, &dir.path), None);
        assert_bounded_passes(&dir, &format!("head -200 {}", file.display()));
    }

    #[test]
    fn test_f1_pre_bash_tail_count_passes_through() {
        let (dir, file) = long_line_project("f1-bash-tail-count");
        let input = bash_tool_input("s1", &format!("tail -20 {}", file.display()));
        assert_eq!(handle_pre_bash(&input, &dir.path), None);
        assert_bounded_passes(&dir, &format!("tail -200 {}", file.display()));
    }

    #[test]
    fn test_f1_pre_bash_head_bytes_passes_through() {
        let (dir, file) = long_line_project("f1-bash-head-bytes");
        let input = bash_tool_input("s1", &format!("head -c 200 {}", file.display()));
        assert_eq!(handle_pre_bash(&input, &dir.path), None);
        assert_bounded_passes(&dir, &format!("head -c 25000 {}", file.display()));
    }

    #[test]
    fn test_f1_post_bash_emits_the_note_after_a_rewritten_cat() {
        let (dir, file) = large_file_project("f1-bash-post-note");
        let original = bash_tool_input("s1", &format!("cat {}", file.display()));
        let out = handle_pre_bash(&original, &dir.path).expect("narrows");
        let json: Value = serde_json::from_str(&out).expect("valid JSON");
        let rewritten = json["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .expect("command")
            .to_string();

        // The post side never sees the original command. A stray call with
        // it must leave the record in place.
        assert_eq!(handle_post_bash(&original, &dir.path), None);
        let input = bash_tool_input("s1", &rewritten);
        let out = handle_post_bash(&input, &dir.path).expect("the note follows a narrowing");
        let json: Value = serde_json::from_str(&out).expect("valid JSON");
        let hso = &json["hookSpecificOutput"];
        assert_eq!(hso["hookEventName"], "PostToolUse");
        let note = hso["additionalContext"].as_str().expect("note text");
        assert!(
            note.contains("fn f0"),
            "the note holds the skeleton: {note}"
        );
        assert!(note.contains("hook replaced your command"), "{note}");
        assert_eq!(
            handle_post_bash(&input, &dir.path),
            None,
            "the record clears after one note"
        );
    }

    #[test]
    fn test_f1_pre_bash_a_failed_record_passes_the_command_through() {
        let (dir, file) = large_file_project("f1-bash-record-fails");
        // A regular file where the session directory belongs makes every
        // session write fail. `write_json_atomic` cannot create the dir.
        let session_dir = cache_dir(&dir.path).join("session");
        std::fs::create_dir_all(session_dir.parent().expect("parent")).expect("mkdir");
        std::fs::write(&session_dir, "not a directory").expect("block the dir");

        let input = bash_tool_input("s1", &format!("cat {}", file.display()));
        assert_eq!(
            handle_pre_bash(&input, &dir.path),
            None,
            "a rewrite with no record must pass the command through"
        );
        let rewritten = bash_tool_input("s1", &narrowed_bash_command(&canonical_key(&file)));
        assert_eq!(handle_post_bash(&rewritten, &dir.path), None);
    }

    #[test]
    fn test_f1_pre_bash_compound_command_passes_through() {
        let (dir, file) = large_file_project("f1-bash-compound");
        let input = bash_tool_input("s1", &format!("cat {} | head", file.display()));
        assert_eq!(handle_pre_bash(&input, &dir.path), None);
        assert!(read_session(&dir.path, "s1").narrowed.is_empty());
    }

    #[test]
    fn test_f1_pre_bash_clears_a_stale_note() {
        let (dir, file) = large_file_project("f1-bash-stale-note");
        let input = bash_tool_input("s1", &format!("cat {}", file.display()));
        handle_pre_bash(&input, &dir.path).expect("narrows");

        // The file shrinks. F1 now passes the cat through in full.
        std::fs::write(&file, "pub fn f0() -> u64 {\n    0\n}\n").expect("rewrite file");
        assert_eq!(handle_pre_bash(&input, &dir.path), None);
        let rewritten = bash_tool_input("s1", &narrowed_bash_command(&canonical_key(&file)));
        assert_eq!(
            handle_post_bash(&rewritten, &dir.path),
            None,
            "a full cat must not get the old note"
        );
    }

    #[test]
    fn test_f1_bash_read_never_touches_an_f3_record() {
        let (dir, big) = large_file_project("f1-bash-f3");
        let small = dir.path.join("src").join("small.rs");
        std::fs::write(&small, "pub fn one() {}\n").expect("write file");
        // A Read of the small file passes and writes its F3 record.
        assert_eq!(
            handle_pre_read(&read_tool_input("s1", &small), &dir.path),
            None
        );
        let before = read_session(&dir.path, "s1").reads;
        assert_eq!(before.len(), 1);

        // A full cat of the small file and a narrowed cat of the big file.
        let full = bash_tool_input("s1", &format!("cat {}", small.display()));
        assert_eq!(handle_pre_bash(&full, &dir.path), None);
        let narrowed = bash_tool_input("s1", &format!("cat {}", big.display()));
        handle_pre_bash(&narrowed, &dir.path).expect("narrows");

        assert_eq!(read_session(&dir.path, "s1").reads, before);
        let second = handle_pre_read(&read_tool_input("s1", &small), &dir.path);
        assert!(
            second.is_some_and(|s| s.contains("\"deny\"")),
            "F3 still denies the second Read after a cat"
        );
    }

    #[test]
    fn test_f1_stop_counts_and_clears_the_lost_notes() {
        let dir = TempDir::new("f1-stop-lost-notes");
        update_session(&dir.path, "s1", |session| {
            session.narrowed.insert("/a.rs".into(), "note a".into());
            session.narrowed.insert("/b.rs".into(), "note b".into());
        });

        handle_stop(&serde_json::json!({ "session_id": "s1" }), &dir.path);

        assert!(read_session(&dir.path, "s1").narrowed.is_empty());
        assert_eq!(read_stats(&dir.path).expect("stats").notes_lost, 2);
    }
}
