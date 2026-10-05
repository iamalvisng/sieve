//! Pins the Sieve product name across every runtime-visible string in
//! `sieve-daemon`.

use std::io::Cursor;
use std::path::Path;

use sieve_daemon::names::{canonical_tool_name, server_name, tool_names};
use sieve_daemon::serve;

/// Runs one `serve` session over the given NDJSON input lines and returns
/// stdout.
fn run(input: &str) -> String {
    // ponytail: skips the real refresh so this test touches no filesystem
    // lock; each test call pins `SIEVE`, so the env var name matches.
    std::env::set_var(sieve_core::product().env_var("NO_REFRESH"), "1");
    let root = Path::new(".");
    let stdin = Cursor::new(input.as_bytes().to_vec());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    // `dir_override = true` keeps `tools/list` full here: this crate dir
    // holds no graph, and an empty list would hold no name to check.
    serve(
        root,
        root,
        true,
        "0.1.0",
        stdin,
        &mut stdout,
        &mut stderr,
        None,
    );
    String::from_utf8(stdout).unwrap()
}

#[test]
fn rename_table_daemon_tool_names_are_sieve() {
    assert_eq!(
        tool_names(),
        [
            "sieve_find_code",
            "sieve_file_api",
            "sieve_check_freshness",
            "sieve_trace_calls",
            "sieve_find_all",
            "sieve_repo_map",
        ]
    );
}

#[test]
fn rename_table_daemon_canonical_tool_name_maps_ask() {
    assert_eq!(canonical_tool_name("sieve_ask"), "sieve_find_code");
}

#[test]
fn rename_table_daemon_server_name_is_sieve() {
    assert_eq!(server_name(), "sieve");
}

#[test]
fn test_f6_sieve_tools_list_adds_sieve_why_and_instructions_name_it() {
    let out = run("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n");
    assert!(out.contains("\"name\":\"sieve_why\""), "reply: {out}");
    let specs = sieve_daemon::names::tool_specs();
    assert_eq!(specs.len(), 7);
    assert_eq!(specs[6].name, "sieve_why");
    assert!(sieve_daemon::names::instructions().contains("- sieve_why: "));
    // `tool_names` keeps the six base tools.
    assert_eq!(tool_names().len(), 6);
}

fn fake_why(symbol: &str, all: bool, _root: &Path, _ctx: &Path) -> (String, bool) {
    (format!("why {symbol} all={all}"), false)
}

#[test]
fn test_f6_sieve_why_call_runs_the_handler_and_needs_a_symbol() {
    sieve_daemon::tools::set_why_handler(fake_why);
    let ok_call = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"sieve_why\",\"arguments\":{\"symbol\":\"abc\"}}}\n";
    let call = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"sieve_why\",\"arguments\":{\"symbol\":\"abc\",\"all\":true}}}\n";
    let out = run(call);
    assert!(out.contains("why abc all=true"), "reply: {out}");
    let call = call.replace("\"symbol\":\"abc\",", "");
    let out = run(&call);
    assert!(out.contains("sieve_why requires a symbol"), "reply: {out}");
    assert!(out.contains("\"isError\":true"), "reply: {out}");
    // A panic in the handler gives an error text. The next line still runs.
    sieve_daemon::tools::set_why_handler(panicking_why);
    let ping = "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n";
    let out = run(&format!("{ok_call}{ping}"));
    assert!(
        out.contains("sieve_why failed: internal error"),
        "reply: {out}"
    );
    assert!(out.contains("\"isError\":true"), "reply: {out}");
    assert!(out.contains("\"id\":9"), "the server stopped: {out}");
}

fn panicking_why(_symbol: &str, _all: bool, _root: &Path, _ctx: &Path) -> (String, bool) {
    panic!("handler failure");
}
