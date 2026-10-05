//! The optional LSP enrichment layer (P2-23 to P2-25).
//!
//! `sieve build --lsp` spawns one language server, asks it for the
//! outgoing calls of every function and method node, and adds a `calls`
//! edge, confidence `lsp_resolved`, for every callee the AST resolver
//! could not place. This layer is opt-in: `enrich_with_lsp` runs only
//! when the caller asks for it, and every failure degrades to a no-op
//! (added 0, the graph unchanged); see the `languages-lsp.md` note
//! section 5.
//!
//! The transport is a hand-rolled JSON-RPC framer (`Content-Length`
//! headers) over `std::process::Command`, so this module adds no crate
//! dependency beyond `serde_json`, already a dependency of this crate.

use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use sieve_core::lang::lang_for_path;
use sieve_core::{Confidence, Edge, Graph, Kind, Node, Relation};

use crate::generic::generic_lang_of;

/// The language name a node's path speaks: the breadth-tier name when one
/// claims the path, else the depth-tier label.
fn lang_of(path: &Path) -> Option<&'static str> {
    let path_str = path.to_string_lossy();
    generic_lang_of(&path_str)
        .map(|lang| lang.name)
        .or_else(|| lang_for_path(path).map(|lang| lang.label))
}

/// One row of the server registry: a language server this layer knows
/// how to spawn, and the languages it covers.
#[derive(Debug, Clone, Copy)]
pub struct ServerSpec {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub languages: &'static [&'static str],
}

/// The server registry, first match wins.
pub const REGISTRY: &[ServerSpec] = &[
    ServerSpec {
        name: "rust-analyzer",
        command: "rust-analyzer",
        args: &[],
        languages: &["rust"],
    },
    ServerSpec {
        name: "clangd",
        command: "clangd",
        args: &["--background-index"],
        languages: &["cpp", "c"],
    },
    ServerSpec {
        name: "gopls",
        command: "gopls",
        args: &[],
        languages: &["go"],
    },
    ServerSpec {
        name: "pyright-langserver",
        command: "pyright-langserver",
        args: &["--stdio"],
        languages: &["python"],
    },
    ServerSpec {
        name: "typescript-language-server",
        command: "typescript-language-server",
        args: &["--stdio"],
        languages: &["typescript", "javascript", "tsx"],
    },
];

/// A registry row whose command resolved to a path on this machine.
#[derive(Debug, Clone)]
pub struct ResolvedServer {
    pub spec: &'static ServerSpec,
    pub command_path: String,
}

/// Timeouts.
const SPAWN_SETTLE: Duration = Duration::from_millis(50);
const INITIALIZE_TIMEOUT: Duration = Duration::from_millis(120_000);
const READY_POLL_TIMEOUT: Duration = Duration::from_millis(90_000);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(2_000);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(15_000);

/// A ceiling on how many source nodes one enrichment run queries.
///
/// ponytail: a flat cap, not a budget the caller can tune. Add a flag
/// when a real repo's node count makes this matter.
const MAX_NODES: usize = 2000;

/// A ceiling on one JSON-RPC message body's declared `Content-Length`
/// (P2-23 F8). A value past this treats the stream as broken rather than
/// allocating whatever size a malformed or hostile server names.
const MAX_MESSAGE_LEN: usize = 64 * 1024 * 1024;

/// A ceiling on one header line's length. A server that never sends a
/// `\r\n` would otherwise grow `read_line`'s buffer without limit; a
/// line past this treats the stream as broken, the same degrade
/// `MAX_MESSAGE_LEN` already takes.
const MAX_HEADER_LINE_LEN: usize = 8 * 1024;

/// The lookup process: `/bin/sh -c "command -v {cmd}"`.
fn lookup_command(cmd: &str, extra_path_dir: Option<&Path>) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!("command -v {cmd}"));
    if let Some(dir) = extra_path_dir {
        let current = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{current}", dir.display()));
    }
    command
}

/// Resolves `cmd` to an absolute path with `command -v`, run through `/bin/sh
/// -c` (P2-24). The shell is not `$SHELL` and not a login shell.
///
/// `extra_path_dir`, when given, is prepended to that shell's `PATH` for
/// this one lookup only. Production callers pass `None`; a test uses it
/// to put a fake server on the login shell's `PATH` without touching the
/// test process's own environment (`std::env::set_var` is `unsafe` and
/// would race other tests in this binary).
fn resolve_command(cmd: &str, extra_path_dir: Option<&Path>) -> Option<String> {
    let output = lookup_command(cmd, extra_path_dir).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// Picks the first registry row, in priority order, whose language set
/// meets `languages` and whose command resolves on this machine (P2-24).
/// Returns `None` when no row matches: the caller then does nothing.
pub fn pick_server(languages: &BTreeSet<String>) -> Option<ResolvedServer> {
    pick_server_with_extra_path(languages, None)
}

/// [`pick_server`], with an extra `PATH` directory tried first (test
/// hook, see [`resolve_command`]).
pub fn pick_server_with_extra_path(
    languages: &BTreeSet<String>,
    extra_path_dir: Option<&Path>,
) -> Option<ResolvedServer> {
    REGISTRY.iter().find_map(|spec| {
        let covers = spec.languages.iter().any(|lang| languages.contains(*lang));
        if !covers {
            return None;
        }
        resolve_command(spec.command, extra_path_dir)
            .map(|command_path| ResolvedServer { spec, command_path })
    })
}

/// The repo's language set, from every file node's path:
/// the breadth-tier name, else the depth-tier label,
/// per node). A breadth-tier repo (rust, c, cpp, ruby, ...) must count
/// here, or `pick_server` never sees past a depth-tier row (P2-24).
pub fn repo_languages(graph: &Graph) -> BTreeSet<String> {
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .filter_map(|n| lang_of(Path::new(&n.path)))
        .map(|lang| lang.to_string())
        .collect()
}

/// One write job for [`write_messages`]: a framed message body, plus a
/// channel to report back whether the write finished.
struct WriteJob {
    body: Vec<u8>,
    ack: mpsc::Sender<std::io::Result<()>>,
}

/// Writes `Content-Length`-framed bodies to `stdin` as jobs arrive on
/// `rx`, one at a time, until a job's write fails or `rx` disconnects
/// (P2-23 F4). Runs on its own thread so a stuck server with a full pipe
/// buffer blocks this thread, never the caller waiting on `job.ack`.
fn write_messages(mut stdin: ChildStdin, rx: Receiver<WriteJob>) {
    while let Ok(job) = rx.recv() {
        let result = (|| -> std::io::Result<()> {
            write!(stdin, "Content-Length: {}\r\n\r\n", job.body.len())?;
            stdin.write_all(&job.body)?;
            stdin.flush()
        })();
        let failed = result.is_err();
        if job.ack.send(result).is_err() || failed {
            return;
        }
    }
}

/// A minimal JSON-RPC 2.0 client for one spawned language server, framed
/// with `Content-Length` headers.
struct LspClient {
    child: Child,
    writer_tx: mpsc::Sender<WriteJob>,
    rx: Receiver<serde_json::Value>,
    next_id: i64,
    /// Set once a write times out or the writer thread is gone (P2-23
    /// F4). Every later `send` degrades without trying to write again.
    dead: bool,
}

impl LspClient {
    /// Spawns `spec.command` with `spec.args`, cwd `root`. Waits
    /// [`SPAWN_SETTLE`] then checks the child has not already exited
    /// (a missing binary or an early crash), per the P2-23 degrade rule.
    fn spawn(command_path: &str, args: &[&str], root: &Path) -> Option<Self> {
        let mut child = Command::new(command_path)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        thread::sleep(SPAWN_SETTLE);
        if child.try_wait().ok().flatten().is_some() {
            // The child already exited: an early-exit degrade (P2-23).
            return None;
        }

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || read_messages(stdout, tx));
        let (writer_tx, writer_rx) = mpsc::channel();
        thread::spawn(move || write_messages(stdin, writer_rx));

        Some(LspClient {
            child,
            writer_tx,
            rx,
            next_id: 1,
            dead: false,
        })
    }

    /// Frames and writes `value`, off-thread, and waits up to
    /// [`REQUEST_TIMEOUT`] for the write itself to finish (P2-23 F4). A
    /// stuck server with a full pipe buffer times out this call rather
    /// than hanging it; a timed-out write marks the client dead, the same
    /// degrade a timed-out read already takes.
    fn send(&mut self, value: &serde_json::Value) -> std::io::Result<()> {
        self.send_with_timeout(value, REQUEST_TIMEOUT)
    }

    fn send_with_timeout(
        &mut self,
        value: &serde_json::Value,
        timeout: Duration,
    ) -> std::io::Result<()> {
        if self.dead {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the LSP client is dead",
            ));
        }
        let body = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.writer_tx.send(WriteJob { body, ack: ack_tx }).is_err() {
            self.dead = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the LSP client's writer thread exited",
            ));
        }
        match ack_rx.recv_timeout(timeout) {
            Ok(result) => {
                if result.is_err() {
                    self.dead = true;
                }
                result
            }
            Err(_) => {
                self.dead = true;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the write to the LSP server's stdin timed out",
                ))
            }
        }
    }

    /// Sends a request and waits up to `timeout` for its response. Any
    /// I/O error, a channel disconnect (the server exited), or a timeout
    /// all resolve to `None` — the P2-23 degrade path.
    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&msg).ok()?;

        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
            match self.rx.recv_timeout(remaining) {
                Ok(value) => {
                    if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
                        return value.get("result").cloned();
                    }
                    // A notification, or a response to an earlier
                    // request this client gave up on: keep waiting.
                }
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) -> std::io::Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.send(&msg)
    }
}

impl Drop for LspClient {
    /// `dispose` kills the child.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads `Content-Length`-framed JSON-RPC messages from `stdout` until
/// EOF, sending each parsed message over `tx`. A framing or JSON error
/// stops the reader; the client's next `recv_timeout` then disconnects,
/// which callers treat as a degrade.
fn read_messages(stdout: impl Read, tx: mpsc::Sender<serde_json::Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match (&mut reader)
                .take(MAX_HEADER_LINE_LEN as u64)
                .read_line(&mut line)
            {
                Ok(0) => return, // EOF
                Ok(_) => {}
                Err(_) => return,
            }
            if !line.ends_with('\n') && line.len() >= MAX_HEADER_LINE_LEN {
                // A header line with no `\r\n` inside the cap: a broken
                // or hostile stream (see `MAX_HEADER_LINE_LEN`).
                return;
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break; // blank line: headers done
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().ok();
            }
        }
        let Some(len) = content_length else { return };
        if len > MAX_MESSAGE_LEN {
            // A broken or hostile stream: stop reading rather than
            // allocate an attacker-chosen size (P2-23 F8). Dropping `tx`
            // disconnects the client's receiver, the same degrade path a
            // framing error already takes.
            return;
        }
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
            return;
        };
        if tx.send(value).is_err() {
            return;
        }
    }
}

/// A file:// URI for an absolute path, plain and ASCII-safe enough for a
/// temp-dir test fixture (no percent-encoding).
fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// The repo-relative posix path for a `file://` URI under `root`, or
/// `None` when the URI names a path outside `root` (external:
/// a leading `..` or an absolute path drops).
fn uri_to_rel(uri: &str, root: &Path) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    let path = Path::new(path);
    let rel = path.strip_prefix(root).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Parses a `L<start>-L<end>` span into its 1-indexed line numbers.
fn parse_span(span: &str) -> Option<(u32, u32)> {
    let rest = span.strip_prefix('L')?;
    let (start, rest) = rest.split_once("-L")?;
    Some((start.parse().ok()?, rest.parse().ok()?))
}

/// The kinds an LSP callee target may resolve to.
fn is_target_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Function
            | Kind::Method
            | Kind::Class
            | Kind::Struct
            | Kind::Interface
            | Kind::Type
            | Kind::Enum
    )
}

/// Finds the innermost node in `path`, among [`is_target_kind`] kinds,
/// whose span covers 1-indexed `line`.
fn innermost_at<'a>(nodes: &'a [Node], path: &str, line: u32) -> Option<&'a Node> {
    nodes
        .iter()
        .filter(|n| n.path == path && is_target_kind(n.kind))
        .filter_map(|n| parse_span(&n.span).map(|(s, e)| (n, s, e)))
        .filter(|(_, s, e)| *s <= line && line <= *e)
        .min_by_key(|(_, s, e)| e - s)
        .map(|(n, _, _)| n)
}

/// The first line of `node`'s span in `source` that holds `node.name`, scanned
/// up to two lines past the span's start. Returns a 0-indexed `(line,
/// character)` LSP position, or `None` when no line in that window holds the
/// name.
fn name_pos(node: &Node, source: &str) -> Option<(u32, u32)> {
    let (start, _) = parse_span(&node.span)?;
    let lines: Vec<&str> = source.lines().collect();
    let last = std::cmp::min(start as usize + 2, lines.len());
    for line_no in start as usize..=last {
        if line_no == 0 || line_no > lines.len() {
            continue;
        }
        let text = lines[line_no - 1];
        if let Some(byte_idx) = text.find(node.name.as_str()) {
            // LSP `character` counts UTF-16 code units, not chars (P2-23
            // F7): an astral-plane character before the name is two units.
            let character = text[..byte_idx].encode_utf16().count() as u32;
            return Some((line_no as u32 - 1, character));
        }
    }
    None
}

/// Runs the LSP enrichment layer over `graph`, mutating it in place, and
/// returns the number of edges added (P2-23 to P2-25). Every failure —
/// no server found, a spawn or initialize timeout, a server that exits
/// early — returns `0` and leaves `graph` untouched.
pub fn enrich_with_lsp(root: &Path, graph: &mut Graph) -> usize {
    enrich_with_lsp_and_extra_path(root, graph, None)
}

/// What one LSP run did. The `--lsp` progress line prints these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspReport {
    /// Edges added.
    pub added: usize,
    /// Source nodes the server answered a call hierarchy for.
    pub queried: usize,
    /// The resolved server path, or `None` when no server was found.
    pub server: Option<String>,
}

/// [`enrich_with_lsp`], returning the full [`LspReport`].
pub fn enrich_with_lsp_report(root: &Path, graph: &mut Graph) -> LspReport {
    enrich_with_lsp_report_and_extra_path(root, graph, None)
}

/// [`enrich_with_lsp`], with an extra `PATH` directory tried first when
/// picking a server (test hook, see [`resolve_command`]).
pub fn enrich_with_lsp_and_extra_path(
    root: &Path,
    graph: &mut Graph,
    extra_path_dir: Option<&Path>,
) -> usize {
    enrich_with_lsp_report_and_extra_path(root, graph, extra_path_dir).added
}

/// [`enrich_with_lsp_report`], with an extra `PATH` directory tried first
/// (test hook, see [`resolve_command`]).
pub fn enrich_with_lsp_report_and_extra_path(
    root: &Path,
    graph: &mut Graph,
    extra_path_dir: Option<&Path>,
) -> LspReport {
    // Canonicalize once, up front. A server reports
    // callee URIs against the real path, so a symlinked checkout (macOS
    // `/tmp` -> `/private/tmp`, common in CI) would otherwise fail
    // `uri_to_rel`'s in-repo test for every callee (P2-23 F1). Fall back to
    // the given root when canonicalize fails; the rest of this run then
    // degrades as it did before this fix.
    let root: std::path::PathBuf =
        std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let root = root.as_path();

    let languages = repo_languages(graph);
    let Some(server) = pick_server_with_extra_path(&languages, extra_path_dir) else {
        return LspReport {
            added: 0,
            queried: 0,
            server: None,
        };
    };
    let none = || LspReport {
        added: 0,
        queried: 0,
        server: Some(server.command_path.clone()),
    };

    let Some(mut client) = LspClient::spawn(&server.command_path, server.spec.args, root) else {
        return none();
    };

    let init_params = serde_json::json!({
        "processId": null,
        "rootUri": file_uri(root),
        "capabilities": {},
    });
    if client
        .request("initialize", init_params, INITIALIZE_TIMEOUT)
        .is_none()
    {
        return none();
    }
    let _ = client.notify("initialized", serde_json::json!({}));

    let handled_langs = server.spec.languages;
    let mut source_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, Kind::Function | Kind::Method))
        .filter(|n| {
            lang_of(Path::new(&n.path))
                .map(|lang| handled_langs.contains(&lang))
                .unwrap_or(false)
        })
        .collect();
    source_nodes.truncate(MAX_NODES);
    if source_nodes.is_empty() {
        return none();
    }

    let mut existing: HashSet<String> = graph
        .edges
        .iter()
        .map(|e| dedupe_key(&e.source, e.relation.as_str(), &e.target))
        .collect();

    let mut opened_files: HashSet<String> = HashSet::new();
    let mut ready = false;
    let mut queried = 0usize;
    let mut new_edges: Vec<Edge> = Vec::new();
    let deadline = std::time::Instant::now() + READY_POLL_TIMEOUT;

    for node in &source_nodes {
        let full_path = root.join(&node.path);
        // Read the source as lossy UTF-8: an invalid byte becomes U+FFFD, never
        // a skip.
        let Ok(bytes) = std::fs::read(&full_path) else {
            continue;
        };
        let source = String::from_utf8_lossy(&bytes).into_owned();
        let Some((line, character)) = name_pos(node, &source) else {
            continue;
        };

        let uri = file_uri(&full_path);
        if opened_files.insert(node.path.clone()) {
            let _ = client.notify(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": lang_for_path(Path::new(&node.path))
                            .map(|l| l.label)
                            .unwrap_or(""),
                        "version": 1,
                        "text": source,
                    }
                }),
            );
        }

        // The warm-up: the first source node with a position drives
        // `waitUntilReady`. A failure to ever get a non-null response
        // returns `{added: 0}`.
        let prepare_params = serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": line, "character": character},
        });
        let items = if ready {
            client.request(
                "textDocument/prepareCallHierarchy",
                prepare_params,
                REQUEST_TIMEOUT,
            )
        } else {
            let result = poll_until_ready(&mut client, &prepare_params, deadline);
            if result.is_some() {
                ready = true;
            }
            result
        };
        if !ready {
            return none();
        }

        let Some(items) = items else { continue };
        let Some(item) = items.as_array().and_then(|a| a.first()) else {
            continue;
        };
        queried += 1;

        let Some(calls) = client.request(
            "callHierarchy/outgoingCalls",
            serde_json::json!({"item": item}),
            REQUEST_TIMEOUT,
        ) else {
            continue;
        };
        let Some(calls) = calls.as_array() else {
            continue;
        };

        for call in calls {
            let Some(to) = call.get("to") else { continue };
            let Some(target_uri) = to.get("uri").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(target_path) = uri_to_rel(target_uri, root) else {
                continue; // external target
            };
            let target_line = to
                .get("selectionRange")
                .or_else(|| to.get("range"))
                .and_then(|r| r.get("start"))
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64());
            let Some(target_line) = target_line else {
                continue;
            };
            let target_line_1indexed = target_line as u32 + 1;

            let Some(target_node) = innermost_at(&graph.nodes, &target_path, target_line_1indexed)
            else {
                continue;
            };
            if target_node.id == node.id {
                continue; // self-loop
            }

            let key = dedupe_key(&node.id, Relation::Calls.as_str(), &target_node.id);
            if !existing.insert(key) {
                continue; // already present, P2-25 dedupe
            }
            new_edges.push(Edge {
                source: node.id.clone(),
                target: target_node.id.clone(),
                relation: Relation::Calls,
                confidence: Confidence::LspResolved,
            });
        }
    }

    let added = new_edges.len();
    graph.edges.extend(new_edges);
    graph.meta.edge_count = graph.edges.len();
    LspReport {
        added,
        queried,
        server: Some(server.command_path),
    }
}

/// Polls `prepareCallHierarchy` every [`READY_POLL_INTERVAL`] until it
/// returns a result or `deadline` passes.
fn poll_until_ready(
    client: &mut LspClient,
    params: &serde_json::Value,
    deadline: std::time::Instant,
) -> Option<serde_json::Value> {
    loop {
        if let Some(result) = client.request(
            "textDocument/prepareCallHierarchy",
            params.clone(),
            REQUEST_TIMEOUT,
        ) {
            return Some(result);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        thread::sleep(
            READY_POLL_INTERVAL.min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
}

/// The exact dedupe key: `source\0relation\0target` (P2-25).
fn dedupe_key(source: &str, relation: &str, target: &str) -> String {
    format!("{source}\0{relation}\0{target}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn test_p2_24_dv22_lookup_runs_bin_sh_c_not_the_login_shell() {
        let command = lookup_command("clangd", None);
        assert_eq!(command.get_program(), "/bin/sh");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-c", "command -v clangd"]);
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pick_server_returns_none_when_no_row_s_language_matches() {
        // No registry row covers Haskell, so this never depends on
        // which servers happen to sit on the test machine's `PATH`.
        // The command-resolution and priority rules (P2-24) get a real
        // pin in `tests/lsp.rs`, with a fake command on `PATH`.
        let langs = set(&["haskell"]);
        assert!(pick_server(&langs).is_none());
    }

    #[test]
    fn registry_lists_rust_analyzer_before_gopls() {
        let rust_pos = REGISTRY.iter().position(|s| s.name == "rust-analyzer");
        let go_pos = REGISTRY.iter().position(|s| s.name == "gopls");
        assert!(rust_pos < go_pos);
    }

    #[test]
    fn test_p2_25_dedupe_key_matches_the_exact_golden_shape() {
        assert_eq!(dedupe_key("a", "calls", "b"), "a\0calls\0b");
    }

    #[test]
    fn parse_span_reads_1_indexed_start_and_end() {
        assert_eq!(parse_span("L3-L7"), Some((3, 7)));
        assert_eq!(parse_span("garbage"), None);
    }

    #[test]
    fn innermost_at_picks_the_smallest_covering_span() {
        let outer = sample_node("f.py#outer", Kind::Function, "f.py", "L1-L10");
        let inner = sample_node("f.py#outer.inner", Kind::Function, "f.py", "L3-L5");
        let nodes = vec![outer, inner];
        let found = innermost_at(&nodes, "f.py", 4).expect("a covering node exists");
        assert_eq!(found.id, "f.py#outer.inner");
    }

    #[test]
    fn name_pos_scans_up_to_two_lines_past_the_span_start() {
        let node = sample_node("f.py#foo", Kind::Function, "f.py", "L2-L4");
        let source = "x = 1\ny = 2\ndef foo():\n    pass\n";
        let pos = name_pos(&node, source).expect("the name is on the third line");
        assert_eq!(pos, (2, 4));
    }

    #[test]
    fn name_pos_returns_none_when_the_window_never_holds_the_name() {
        let node = sample_node("f.py#missing", Kind::Function, "f.py", "L1-L1");
        let source = "x = 1\ny = 2\ny = 3\ny = 4\n";
        assert_eq!(name_pos(&node, source), None);
    }

    #[test]
    fn test_p2_23_f7_name_pos_counts_utf_16_units_not_chars() {
        // "\u{1F600}" (an astral-plane emoji) is one `char` but two UTF-16
        // code units, the unit LSP's `character` field counts. It sits
        // before the name on the same line, so a `chars().count()` bug
        // would report `character: 2`, one short of the real `3`.
        let node = sample_node("f.py#foo", Kind::Function, "f.py", "L1-L1");
        let source = "\u{1F600} foo\n";
        let pos = name_pos(&node, source).expect("the name is on the first line");
        assert_eq!(pos, (0, 3));
    }

    /// A fake child that never reads its stdin (P2-23 F4): a write past
    /// the OS pipe buffer must time out, not hang the test.
    #[test]
    fn test_p2_23_f4_a_write_to_a_stuck_child_times_out_and_marks_the_client_dead() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawns sleep as a stand-in for a stuck server");

        let stdin = child.stdin.take().expect("sleep's stdin is piped");
        let stdout = child.stdout.take().expect("sleep's stdout is piped");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || read_messages(stdout, tx));
        let (writer_tx, writer_rx) = mpsc::channel();
        thread::spawn(move || write_messages(stdin, writer_rx));
        let mut client = LspClient {
            child,
            writer_tx,
            rx,
            next_id: 1,
            dead: false,
        };

        // A body well past a pipe's default buffer (commonly 64 KiB):
        // `sleep` never reads, so the write blocks until this deadline.
        let big = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "noop",
            "params": "x".repeat(8 * 1024 * 1024),
        });
        let result = client.send_with_timeout(&big, Duration::from_millis(200));
        assert!(result.is_err(), "a write past the deadline is an error");
        assert!(client.dead, "a timed-out write marks the client dead");

        let again = client.send_with_timeout(&serde_json::json!({}), Duration::from_millis(50));
        assert!(again.is_err(), "a dead client refuses every later write");
    }

    /// A header line with no `\r\n`, longer than [`MAX_HEADER_LINE_LEN`]:
    /// the reader must stop rather than grow the line without limit.
    #[test]
    fn test_read_messages_stops_on_a_header_line_past_the_cap() {
        let body = vec![b'x'; MAX_HEADER_LINE_LEN * 4];
        let (tx, rx) = mpsc::channel();
        read_messages(std::io::Cursor::new(body), tx);
        assert!(
            rx.try_recv().is_err(),
            "an unterminated header line sends no message"
        );
    }

    fn sample_node(id: &str, kind: Kind, path: &str, span: &str) -> Node {
        let name = id.rsplit(['.', '#']).next().unwrap_or(id).to_string();
        Node {
            id: id.to_string(),
            name,
            kind,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: true,
            origin: sieve_core::Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            summary_state: sieve_core::SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
            arity: None,
            variadic: None,
        }
    }
}
