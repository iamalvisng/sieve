//! Parity tests for `sieve viz --export` (P1-65, step 1) against the
//! goldens under `tests/fixtures/<name>.expected/viz/`.
//!
//! Each test rebuilds the tree the viewer reads from the same goldens: the
//! fixture source, `<name>.expected/sieve/**/*.md` and
//! `<name>.expected/wiring.json` as `sieve/.graph/wiring.json`, in a copy
//! named after the fixture (the page embeds the root's basename). It then
//! byte-compares stdout, stderr, the exit code and `index.html`.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use support::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("read file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Drops every `⬆` update-nudge line.
fn normalize_stderr(s: &str) -> String {
    let kept: Vec<&str> = s.lines().filter(|l| !l.starts_with('⬆')).collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

/// Replaces every spelling of `root` with `<TMP>`.
fn mask_tmp(s: &str, root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    s.replace(&canon.display().to_string(), "<TMP>")
        .replace(&root.display().to_string(), "<TMP>")
}

fn fixtures_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures")
}

/// A scratch dir holding `<name>/` (the built tree, from the goldens) and
/// `nograph/` (the fixture source with no build).
fn setup(name: &str) -> TempDir {
    let temp = TempDir::new(&format!("viz-{name}"));
    let fixture = fixtures_dir().join(name);
    let golden = fixtures_dir().join(format!("{name}.expected"));
    let root = temp.join(name);
    copy_dir(&fixture, &root);
    copy_dir(&golden.join("sieve"), &root.join("sieve"));
    fs::create_dir_all(root.join("sieve/.graph")).expect("create .graph");
    fs::copy(
        golden.join("wiring.json"),
        root.join("sieve/.graph/wiring.json"),
    )
    .expect("copy wiring.json");
    copy_dir(&fixture, &temp.join("nograph"));
    temp
}

fn run_viz(cwd: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .arg("viz")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run sieve viz")
}

/// Runs one golden case in `<temp>/<dir>` and compares every capture.
fn assert_case(name: &str, temp: &Path, dir: &str, id: &str, args: &[&str]) {
    let golden = fixtures_dir().join(format!("{name}.expected/viz"));
    let read = |suffix: &str| {
        fs::read_to_string(golden.join(format!("{id}.{suffix}")))
            .unwrap_or_else(|e| panic!("read golden {id}.{suffix}: {e}"))
    };
    let out = run_viz(&temp.join(dir), args);
    let stdout = mask_tmp(&String::from_utf8_lossy(&out.stdout), temp);
    let stderr = normalize_stderr(&mask_tmp(&String::from_utf8_lossy(&out.stderr), temp));
    let page = golden.join(format!("{id}.index.html"));
    let written = temp.join(dir).join(id).join("index.html");
    if support::golden::blessing() {
        support::golden::bless_triple(
            &golden,
            id,
            stdout.as_bytes(),
            stderr.as_bytes(),
            out.status.code().unwrap_or(-1),
        );
        if written.exists() {
            support::golden::bless(&page, &fs::read(&written).expect("read written page"));
        } else {
            let _ = fs::remove_file(&page);
        }
    }
    assert_eq!(
        support::mask_kb(&stdout),
        support::mask_kb(&read("stdout.txt")),
        "{name}/{id} stdout"
    );
    assert_eq!(stderr, read("stderr.txt"), "{name}/{id} stderr");
    assert_eq!(
        out.status.code().unwrap_or(-1).to_string(),
        read("exit.txt").trim(),
        "{name}/{id} exit"
    );
    if page.is_file() {
        // The viewer is original code, so only the data block must match.
        let want = support::data_block(&fs::read(&page).expect("read golden page")).into_bytes();
        let got = support::data_block(&fs::read(&written).expect("read written page")).into_bytes();
        if want != got {
            let (w, g) = (
                String::from_utf8_lossy(&want),
                String::from_utf8_lossy(&got),
            );
            let at = w
                .char_indices()
                .zip(g.chars())
                .find(|((_, a), b)| a != b)
                .map(|((i, _), _)| i)
                .unwrap_or(w.len().min(g.len()));
            let lo = at.saturating_sub(120);
            panic!(
                "{name}/{id} index.html differs at byte {at} (want {} bytes, got {} bytes)\nwant: {:?}\ngot:  {:?}",
                want.len(),
                got.len(),
                &w[lo..(at + 120).min(w.len())],
                &g[lo..(at + 120).min(g.len())]
            );
        }
    } else {
        assert!(!written.exists(), "{name}/{id} wrote a page sieve did not");
    }
}

fn assert_fixture(name: &str) {
    let temp = setup(name);
    assert_case(name, &temp, name, "export", &["--export", "export"]);
    assert_case(
        name,
        &temp,
        name,
        "tabs",
        &["--export", "tabs", "--tabs", "context", "--title", "PR #1"],
    );
    assert_case(
        name,
        &temp,
        name,
        "badtabs",
        &["--export", "badtabs", "--tabs", "bogus"],
    );
    assert_case(
        name,
        &temp,
        "nograph",
        "nograph",
        &[".", "--export", "nograph"],
    );
}

#[test]
fn test_p1_65_viz_export_matches_golden_on_edges() {
    assert_fixture("edges");
}

#[test]
fn test_p1_65_viz_export_matches_golden_on_deep() {
    assert_fixture("deep");
}

/// A running `sieve viz --no-open` child. It dies with the guard.
struct Live {
    child: std::process::Child,
    /// The first stdout line.
    line: String,
    port: u16,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A port that was free a moment ago.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    l.local_addr().expect("addr").port()
}

/// Starts the server in `cwd` and waits (10 s at most) for its first line.
/// Every call passes `--no-open`: a test never opens a browser.
fn start(cwd: &Path, port: u16) -> Live {
    use std::io::BufRead;
    let mut child = support::sieve_command()
        .args(["viz", "--no-open", "-p", &port.to_string()])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sieve viz");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let mut guard = Live {
        child,
        line: String::new(),
        port: 0,
    };
    guard.line = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("first stdout line");
    let url = guard.line.split("http://127.0.0.1:").nth(1).expect("url");
    guard.port = url
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|d| d.parse().ok())
        .expect("port");
    guard
}

/// One GET: returns the status line, the headers (lowercased) and the body.
fn get(port: u16, target: &str) -> (String, String, Vec<u8>) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("timeout");
    write!(
        s,
        "GET {target} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    )
    .expect("send");
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).expect("read");
    let at = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("head end");
    let head = String::from_utf8_lossy(&raw[..at]).to_lowercase();
    let (status, headers) = head.split_once("\r\n").expect("status line");
    (
        status.to_string(),
        headers.to_string(),
        raw[at + 4..].to_vec(),
    )
}

fn content_type(headers: &str) -> String {
    headers
        .lines()
        .find_map(|l| l.strip_prefix("content-type: "))
        .expect("content-type")
        .to_string()
}

/// Every fixed route of the viewer server: status, content type
/// and body, on the `edges` fixture.
#[test]
fn test_p1_65_viz_serves_the_golden_routes() {
    let temp = setup("edges");
    let live = start(&temp.join("edges"), free_port());
    assert_eq!(
        live.line,
        format!(
            "sieve viz serves http://127.0.0.1:{} \u{b7} press ctrl-c to stop\n",
            live.port
        )
    );
    let asset = |name: &str| {
        fs::read(support::manifest_dir().join("assets/viewer").join(name)).expect("asset")
    };
    for (path, file, ctype) in [
        ("/", "index.html", "text/html; charset=utf-8"),
        ("/index.html", "index.html", "text/html; charset=utf-8"),
        ("/app.js", "app.js", "text/javascript; charset=utf-8"),
        ("/style.css", "style.css", "text/css; charset=utf-8"),
        ("/app.js?v=1", "app.js", "text/javascript; charset=utf-8"),
    ] {
        let (status, headers, body) = get(live.port, path);
        assert_eq!(status, "http/1.1 200 ok", "{path}");
        assert_eq!(content_type(&headers), ctype, "{path}");
        assert_eq!(body, asset(file), "{path}");
    }
    let (status, headers, body) = get(live.port, "/api/context-graph");
    assert_eq!(status, "http/1.1 200 ok");
    assert_eq!(content_type(&headers), "application/json; charset=utf-8");
    let doc: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(doc["meta"]["repoName"], "edges");
    assert_eq!(
        doc["meta"]["nodeCount"],
        doc["nodes"].as_array().expect("nodes").len()
    );
    let (status, headers, body) = get(live.port, "/api/code-graph");
    assert_eq!(status, "http/1.1 200 ok");
    assert_eq!(content_type(&headers), "application/json; charset=utf-8");
    let want = fs::read(fixtures_dir().join("edges.expected/wiring.json")).expect("golden");
    let want: serde_json::Value = serde_json::from_slice(&want).expect("json");
    let got: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(got, want);
    for path in ["/nope", "/../Cargo.toml", "/%2e%2e/Cargo.toml", "/app.js/"] {
        let (status, headers, body) = get(live.port, path);
        assert_eq!(status, "http/1.1 404 not found", "{path}");
        assert_eq!(content_type(&headers), "text/plain", "{path}");
        assert_eq!(body, b"not found", "{path}");
    }
}

/// `/api/code-graph` is a 404 with the message when the wiring graph
/// is missing, and the SSE route opens with `: connected`, then reports a
/// change in the context dir.
#[test]
fn test_p1_65_viz_code_graph_404_and_events_stream() {
    use std::io::{Read, Write};
    let temp = setup("edges");
    fs::remove_file(temp.join("edges/sieve/.graph/wiring.json")).expect("rm wiring");
    let live = start(&temp.join("edges"), free_port());
    let (status, headers, body) = get(live.port, "/api/code-graph");
    assert_eq!(status, "http/1.1 404 not found");
    assert_eq!(content_type(&headers), "application/json; charset=utf-8");
    assert_eq!(
        String::from_utf8_lossy(&body),
        "{\"error\":\"no index in this context dir — run `sieve build` first\"}"
    );
    let mut s = std::net::TcpStream::connect(("127.0.0.1", live.port)).expect("connect");
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("timeout");
    write!(s, "GET /events HTTP/1.1\r\nHost: x\r\n\r\n").expect("send");
    let mut buf = [0u8; 512];
    let mut got = String::new();
    while !got.contains(": connected\n\n") {
        let n = s.read(&mut buf).expect("read");
        assert!(n > 0, "stream closed early: {got:?}");
        got.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(got
        .to_lowercase()
        .contains("content-type: text/event-stream"));
    fs::write(temp.join("edges/sieve/zz-new.md"), "x").expect("write");
    while !got.contains("data: change\n\n") {
        let n = s.read(&mut buf).expect("read change");
        assert!(n > 0, "stream closed early: {got:?}");
        got.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
}

/// A huge or odd `-p` value is an error line, never a panic.
#[test]
fn test_p1_65_viz_bad_port_is_an_error_not_a_panic() {
    let temp = setup("edges");
    for port in ["4294967295", "65536", "-1", "abc"] {
        let out = support::sieve_command()
            .args(["viz", "--no-open", "-p", port])
            .current_dir(temp.join("edges"))
            .output()
            .expect("run sieve viz");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{port}: {stderr}");
        assert!(
            stderr.contains(&format!("sieve: invalid port \"{port}\"")),
            "{port}: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "{port}: {stderr}");
    }
}

/// A busy port moves the server up, and the line names the
/// port it took. The `sieve` name shows in the line.
#[test]
fn test_p1_65_viz_busy_port_takes_the_next_one() {
    let temp = setup("edges");
    // The context-dir check runs first, so the `sieve` name needs `sieve/`.
    copy_dir(&temp.join("edges/sieve"), &temp.join("edges/sieve"));
    let held = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("hold");
    let busy = held.local_addr().expect("addr").port();
    let live = start(&temp.join("edges"), busy);
    assert!(live.port > busy && live.port < busy + 10, "{}", live.port);
    assert_eq!(
        live.line,
        format!(
            "sieve viz serves http://127.0.0.1:{} \u{b7} press ctrl-c to stop\n",
            live.port
        )
    );
    assert_eq!(get(live.port, "/").0, "http/1.1 200 ok");
}

/// Under the `sieve` name the printed line and the build hint carry the
/// product name; the page keeps `window.__SIEVE_DATA__`, which `app.js`
/// reads.
#[test]
fn test_p1_65_viz_export_under_the_sieve_name() {
    let temp = TempDir::new("viz-sieve");
    let root = temp.join("edges");
    copy_dir(&fixtures_dir().join("edges"), &root);
    let run = |dir: &Path| {
        support::sieve_command()
            .args(["viz", "--export", "out"])
            .current_dir(dir)
            .output()
            .expect("run sieve viz")
    };
    let out = run(&root);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        normalize_stderr(&mask_tmp(&String::from_utf8_lossy(&out.stderr), &temp)),
        "sieve: no index at <TMP>/edges/sieve \u{2014} run sieve build\n"
    );
    let sieve_golden = fixtures_dir().join("edges.expected");
    copy_dir(&sieve_golden.join("sieve"), &root.join("sieve"));
    fs::create_dir_all(root.join("sieve/.graph")).expect("create .graph");
    fs::copy(
        sieve_golden.join("wiring.json"),
        root.join("sieve/.graph/wiring.json"),
    )
    .expect("copy wiring.json");
    let out = run(&root);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        support::mask_kb(&mask_tmp(&String::from_utf8_lossy(&out.stdout), &temp)),
        "sieve viz wrote <TMP>/edges/out/index.html \u{b7} <KB> kB \u{b7} 1 concepts \u{b7} 36 symbols\n"
    );
    let page = fs::read(root.join("out/index.html")).expect("read page");
    let want = fs::read(sieve_golden.join("viz/export.index.html")).expect("read golden");
    assert_eq!(
        support::data_block(&page),
        support::data_block(&want),
        "the data block is the same bytes under both names"
    );
}

/// The three viewer files are original code: no license header, no CDN or
/// other external URL, and no name of the tool they replace.
#[test]
fn test_p1_65_viewer_files_are_original_and_offline() {
    let dir = support::manifest_dir().join("assets/viewer");
    for name in ["index.html", "style.css", "app.js"] {
        let text = fs::read_to_string(dir.join(name)).expect("viewer file");
        let low = text.to_lowercase();
        for banned in [
            "http://",
            "https://",
            "//cdn",
            "copyright",
            "license",
            "permission is hereby",
            "d3-",
            "@nanonets",
        ] {
            assert!(!low.contains(banned), "{name} contains {banned:?}");
        }
        // The bare word "d3" as a token, such as `d3.select`.
        assert!(
            !low.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|t| t == "d3"),
            "{name} names d3"
        );
    }
}

/// The exported page is standalone: no `/app.js` or `/style.css` reference,
/// one inline module script, and that script reads `__SIEVE_DATA__`.
#[test]
fn test_p1_65_export_page_is_standalone() {
    let temp = TempDir::new("viz-standalone");
    let root = temp.join("edges");
    copy_dir(&fixtures_dir().join("edges"), &root);
    let golden = fixtures_dir().join("edges.expected");
    copy_dir(&golden.join("sieve"), &root.join("sieve"));
    fs::create_dir_all(root.join("sieve/.graph")).expect("create .graph");
    fs::copy(
        golden.join("wiring.json"),
        root.join("sieve/.graph/wiring.json"),
    )
    .expect("copy wiring.json");
    let out = support::sieve_command()
        .args(["viz", "--export", "out"])
        .current_dir(&root)
        .output()
        .expect("run sieve viz");
    assert_eq!(out.status.code(), Some(0));
    let page = fs::read_to_string(root.join("out/index.html")).expect("read page");
    assert!(!page.contains("/app.js"), "page still links /app.js");
    assert!(!page.contains("/style.css"), "page still links /style.css");
    let at = page
        .find("<script type=\"module\">")
        .expect("inline module script");
    let script = &page[at..];
    assert!(script.contains("window.__SIEVE_DATA__"));
}

/// Every element id that `app.js` looks up with `part("<id>")` or
/// `getElementById("<id>")` has an `id="<id>"` attribute in `index.html`.
/// A missing id makes the viewer fail at load, so this catches a rename in
/// one file only. The test reads both files at run time.
#[test]
fn test_p1_65_every_id_that_app_js_looks_up_exists_in_index_html() {
    let dir = support::manifest_dir().join("assets/viewer");
    let js = fs::read_to_string(dir.join("app.js")).expect("read app.js");
    let html = fs::read_to_string(dir.join("index.html")).expect("read index.html");
    let mut ids = Vec::new();
    for call in ["part(\"", "getElementById(\""] {
        let mut rest = js.as_str();
        while let Some(at) = rest.find(call) {
            let after = &rest[at + call.len()..];
            let end = after.find('"').expect("a closing quote");
            ids.push(after[..end].to_string());
            rest = &after[end..];
        }
    }
    assert!(!ids.is_empty(), "app.js looks up no id");
    for id in ids {
        assert!(
            html.contains(&format!("id=\"{id}\"")),
            "app.js looks up the id {id:?}, and index.html has no such id"
        );
    }
}
