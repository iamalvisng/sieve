//! The live server behind `sieve viz [dir]` (P1-65, step 3). It binds
//! `127.0.0.1` only, and answers a fixed set of routes. A request path never
//! reaches the disk: every file name below is a constant.
//!
//! Routes: `/` and `/index.html`, `/app.js`, `/style.css`,
//! `/api/context-graph`, `/api/code-graph`, `/events`. Anything else is `404
//! not found`.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::Value;
use sieve_query::viz::{assemble_context_graph, ContextEdge, ContextNode, Viewer};

/// Sieve tries this many ports, from the requested one up.
pub(crate) const PORT_ATTEMPTS: u16 = 10;
/// The request head must arrive within this time, in total.
const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// The most connections served at once; more are dropped.
const MAX_CONNECTIONS: usize = 256;
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// The `/events` poll period, in milliseconds.
const WATCH_MS: u64 = 300;
const MAX_REQUEST: usize = 16 * 1024;

const JSON: &str = "application/json; charset=utf-8";
const TEXT: &str = "text/plain";

/// What the server reads on each request.
#[derive(Clone)]
pub(crate) struct Site {
    pub viewer: Viewer<'static>,
    pub context_dir: PathBuf,
    pub repo_name: String,
}

/// One answer: status, content type, body.
pub(crate) type Reply = (u16, &'static str, Vec<u8>);

#[derive(Serialize)]
struct Meta<'a> {
    #[serde(rename = "nodeCount")]
    node_count: usize,
    #[serde(rename = "edgeCount")]
    edge_count: usize,
    #[serde(rename = "skippedFiles")]
    skipped_files: usize,
    #[serde(rename = "droppedEdges")]
    dropped_edges: usize,
    #[serde(rename = "repoName")]
    repo_name: &'a str,
}

#[derive(Serialize)]
struct Doc<'a> {
    meta: Meta<'a>,
    nodes: &'a [ContextNode],
    edges: &'a [ContextEdge],
}

fn json(status: u16, body: String) -> Reply {
    (status, JSON, body.into_bytes())
}

fn err_json(msg: &str) -> Reply {
    json(404, format!("{{\"error\":{}}}", Value::String(msg.into())))
}

/// `GET /api/code-graph`: `<context>/.graph/wiring.json` when it parses
/// and `meta.version` is 1.
fn code_graph(context_dir: &Path) -> Reply {
    let Ok(bytes) = std::fs::read(context_dir.join(".graph").join("wiring.json")) else {
        return err_json("no index in this context dir — run `sieve build` first");
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return err_json("wiring.json is unreadable — regenerate with `sieve build`");
    };
    if value.pointer("/meta/version") != Some(&Value::from(1)) {
        return err_json("wiring.json has an unsupported version — regenerate with `sieve build`");
    }
    // Raw passthrough: any JSON with `meta.version` 1, extra keys kept.
    (200, JSON, bytes)
}

/// Answers one request path (query string already cut off).
pub(crate) fn respond(path: &str, site: &Site) -> Reply {
    let html = "text/html; charset=utf-8";
    match path {
        "/" | "/index.html" => (200, html, site.viewer.html.as_bytes().to_vec()),
        "/app.js" => (
            200,
            "text/javascript; charset=utf-8",
            site.viewer.js.as_bytes().to_vec(),
        ),
        "/style.css" => (
            200,
            "text/css; charset=utf-8",
            site.viewer.css.as_bytes().to_vec(),
        ),
        "/api/context-graph" => {
            let g = assemble_context_graph(&site.context_dir);
            let doc = Doc {
                meta: Meta {
                    node_count: g.node_count,
                    edge_count: g.edge_count,
                    skipped_files: g.skipped_files,
                    dropped_edges: g.dropped_edges,
                    repo_name: &site.repo_name,
                },
                nodes: &g.nodes,
                edges: &g.edges,
            };
            match serde_json::to_string(&doc) {
                Ok(body) => json(200, body),
                Err(e) => (500, TEXT, e.to_string().into_bytes()),
            }
        }
        "/api/code-graph" => code_graph(&site.context_dir),
        _ => (404, TEXT, b"not found".to_vec()),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

/// Reads the request head within `HEAD_DEADLINE` in total and returns the
/// path without its query. `Err(Some(431))` means the head is over the
/// cap; `Err(None)` means drop the connection without a reply.
fn read_path(stream: &mut TcpStream) -> Result<String, Option<u16>> {
    let deadline = Instant::now() + HEAD_DEADLINE;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > MAX_REQUEST {
            return Err(Some(431));
        }
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(None)?;
        stream.set_read_timeout(Some(left)).map_err(|_| None)?;
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(None),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let target = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .ok_or(None)?;
    Ok(target.split('?').next().unwrap_or("/").to_string())
}

type Snapshot = Vec<(String, u64, Option<SystemTime>)>;

/// A cheap fingerprint of the context dir's top level: name, size and
/// mtime of every entry. It is not recursive either.
fn snapshot(dir: &Path) -> Snapshot {
    let mut v: Snapshot = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let m = e.metadata().ok();
            (
                e.file_name().to_string_lossy().into_owned(),
                m.as_ref().map_or(0, |m| m.len()),
                m.and_then(|m| m.modified().ok()),
            )
        })
        .collect();
    v.sort();
    v
}

/// `GET /events`: an SSE stream. It sends `data: change` when the context
/// dir changes, until the client closes. The snapshot
/// comes first, so a change after it is never lost.
// ponytail: each stream scans the context dir every 300 ms. At the 256
// connection cap that is 256 scans per tick; share one watcher if it matters.
fn events(stream: TcpStream, dir: &Path) {
    let seen = snapshot(dir);
    events_from(stream, dir, seen);
}

/// The stream loop of `events`, from a snapshot taken by the caller.
fn events_from(mut stream: TcpStream, dir: &Path, mut seen: Snapshot) {
    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n: connected\n\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    // The read timeout is the poll period; a zero-byte read is a close.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(WATCH_MS)));
    let mut byte = [0u8; 64];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
        let now = snapshot(dir);
        if now != seen {
            seen = now;
            if stream.write_all(b"data: change\n\n").is_err() {
                return;
            }
        }
    }
}

fn handle(mut stream: TcpStream, site: &Site) {
    let (status, ctype, body) = match read_path(&mut stream) {
        Ok(path) if path == "/events" => {
            events(stream, &site.context_dir);
            return;
        }
        Ok(path) => respond(&path, site),
        Err(Some(status)) => (status, TEXT, Vec::new()),
        Err(None) => return,
    };
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(&body));
    let _ = stream.shutdown(Shutdown::Write);
}

/// Binds `127.0.0.1` on `port`, then on each of the next nine ports when
/// one is busy. Returns the listener and the port that
/// The URL names. Any bind error but "address in use" is returned.
/// A port past 65535 counts as busy.
pub(crate) fn bind(port: u16) -> Result<(TcpListener, u16), String> {
    for p in (0..PORT_ATTEMPTS).filter_map(|i| port.checked_add(i)) {
        match TcpListener::bind(("127.0.0.1", p)) {
            Ok(l) => return Ok((l, p)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!(
        "no free port in {port}\u{2013}{} \u{2014} try --port with another number",
        u32::from(port) + u32::from(PORT_ATTEMPTS) - 1
    ))
}

/// Serves until the process stops. One thread answers each connection,
/// and a connection past `MAX_CONNECTIONS` is dropped.
pub(crate) fn serve(listener: &TcpListener, site: &Site) {
    for stream in listener.incoming().flatten() {
        if LIVE.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            LIVE.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let site = site.clone();
        std::thread::spawn(move || {
            handle(stream, &site);
            LIVE.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Opens `url` in the default browser. The caller skips this for `--no-open`.
pub(crate) fn open_browser(url: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/c", "start"]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    // A missing opener is not an error: the URL is already on stdout.
    let _ = cmd
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(dir: &Path) -> Site {
        Site {
            viewer: Viewer {
                html: "<h1>x</h1>",
                css: "b{}",
                js: "1",
            },
            context_dir: dir.to_path_buf(),
            repo_name: "r".into(),
        }
    }

    #[test]
    fn test_p1_65_respond_serves_fixed_routes_and_never_a_disk_path() {
        let dir = std::env::temp_dir().join("sieve-viz-respond-none");
        let s = site(&dir);
        let (st, ct, body) = respond("/", &s);
        assert_eq!((st, ct), (200, "text/html; charset=utf-8"));
        assert_eq!(body, b"<h1>x</h1>");
        assert_eq!(respond("/app.js", &s).1, "text/javascript; charset=utf-8");
        assert_eq!(respond("/style.css", &s).1, "text/css; charset=utf-8");
        for p in ["/../Cargo.toml", "/etc/passwd", "/app.js/", "/x"] {
            assert_eq!(respond(p, &s), (404, TEXT, b"not found".to_vec()), "{p}");
        }
        let (st, ct, body) = respond("/api/code-graph", &s);
        assert_eq!((st, ct), (404, JSON));
        assert_eq!(
            String::from_utf8_lossy(&body),
            "{\"error\":\"no index in this context dir — run `sieve build` first\"}"
        );
        let (st, _, body) = respond("/api/context-graph", &s);
        assert_eq!(st, 200);
        assert_eq!(
            String::from_utf8_lossy(&body),
            "{\"meta\":{\"nodeCount\":0,\"edgeCount\":0,\"skippedFiles\":0,\"droppedEdges\":0,\"repoName\":\"r\"},\"nodes\":[],\"edges\":[]}"
        );
    }

    #[test]
    fn test_p1_65_bind_skips_a_busy_port() {
        // Hold the busy port inside the test. Keep every held listener
        // alive, and skip a port too near the top, so the probe range fits.
        let mut held = Vec::new();
        let busy = loop {
            let l = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
            let port = l.local_addr().expect("addr").port();
            held.push(l);
            if port <= u16::MAX - PORT_ATTEMPTS {
                break port;
            }
        };
        let (_l, got) = bind(busy).expect("bind next");
        assert!(got > busy && got < busy + PORT_ATTEMPTS, "{got}");
    }

    #[test]
    fn test_p1_65_bind_never_overflows_at_the_top_port() {
        // Whatever the outcome, no panic: 65535 is the last port tried.
        match bind(u16::MAX) {
            Ok((_l, p)) => assert_eq!(p, u16::MAX),
            Err(e) => assert_eq!(e, "no free port in 65535–65544"),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sieve-viz-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".graph")).expect("mkdir");
        d
    }

    #[test]
    fn test_p1_65_code_graph_passes_raw_bytes_and_reports_bad_files() {
        let d = scratch("wiring");
        let wiring = d.join(".graph/wiring.json");
        let raw = br#"{"meta":{"version":1,"extra":true},"nodes":[],"zz":{"a":1}}"#;
        std::fs::write(&wiring, raw).expect("write");
        assert_eq!(code_graph(&d), (200, JSON, raw.to_vec()));
        let err = |text: &str| format!("{{\"error\":\"{text}\"}}").into_bytes();
        std::fs::write(&wiring, r#"{"meta":{"version":2}}"#).expect("write");
        assert_eq!(
            code_graph(&d),
            (
                404,
                JSON,
                err("wiring.json has an unsupported version — regenerate with `sieve build`")
            )
        );
        std::fs::write(&wiring, "{not json").expect("write");
        assert_eq!(
            code_graph(&d),
            (
                404,
                JSON,
                err("wiring.json is unreadable — regenerate with `sieve build`")
            )
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A pair of connected loopback streams: (client, server side).
    fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let c = TcpStream::connect(l.local_addr().expect("addr")).expect("connect");
        let (s, _) = l.accept().expect("accept");
        c.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        (c, s)
    }

    #[test]
    fn test_p1_65_events_reports_a_change_after_the_snapshot() {
        let d = scratch("events");
        let seen = snapshot(&d);
        // The change lands after the snapshot and before the stream starts.
        std::fs::write(d.join("new.md"), "x").expect("write");
        let (mut client, server) = pair();
        let dir = d.clone();
        std::thread::spawn(move || events_from(server, &dir, seen));
        let mut got = String::new();
        let mut buf = [0u8; 256];
        while !got.contains("data: change\n\n") {
            let n = client.read(&mut buf).expect("read");
            assert!(n > 0, "closed early: {got:?}");
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn test_p1_65_oversize_head_gets_431() {
        let (mut client, server) = pair();
        let site = site(&std::env::temp_dir());
        std::thread::spawn(move || handle(server, &site));
        client
            .write_all(&vec![b'a'; MAX_REQUEST + 1])
            .expect("send");
        let mut got = String::new();
        client.read_to_string(&mut got).expect("read");
        assert!(
            got.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
            "{got:?}"
        );
    }
}
