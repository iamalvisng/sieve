//! The MCP server identity: the server name, the six tool names, their
//! descriptions and schemas, the legacy aliases, and the boot `instructions`
//! text (the `mcp-server.md` note sections 2 and 3).
//!
//! Every name below routes through `sieve_core::product()`.
//!
//! Every `input_schema` below is a literal, already-compact JSON string,
//! not a `serde_json::Value`: this workspace's `serde_json` has no
//! `preserve_order` feature, so a `Value` object re-serializes its keys
//! alphabetically. The golden pins an exact key order, so the schema text
//! is kept pre-serialized, copied byte for byte from the golden. No
//! `input_schema` holds a product name, so none needs branding.

use sieve_core::product;

/// The `serverInfo.name` field `initialize` reports.
pub fn server_name() -> &'static str {
    product().server_name()
}

/// The six canonical tool names, in `tools/list` order.
pub fn tool_names() -> [String; 6] {
    [
        product().tool("find_code"),
        product().tool("file_api"),
        product().tool("check_freshness"),
        product().tool("trace_calls"),
        product().tool("find_all"),
        product().tool("repo_map"),
    ]
}

/// The name of the `why` tool: `sieve_why`.
pub fn why_tool_name() -> String {
    product().tool("why")
}

/// One `tools/list` entry. `input_schema` holds no product name, so it
/// stays a static, pre-serialized JSON string.
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: &'static str,
}

/// The `tools/list` table, in the exact key order the golden pins.
pub fn tool_specs() -> Vec<ToolSpec> {
    let raw: [(&str, &str, &str); 6] = [
        (
            "sieve_find_code",
            "Query the repo context graph in plain words. Returns ranked nodes with exact file:line spans and the relevant source inlined — usually the full answer, no file reads needed.",
            r#"{"type":"object","properties":{"query":{"type":"string","description":"what you want to understand, in plain words"},"limit":{"type":"number","description":"max results (default 5)"},"full":{"type":"boolean","description":"inline whole definition spans instead of the default ≤8-line crux excerpts"},"in":{"type":"string","description":"narrow to nodes under this path prefix, filtered before scoring (segment-aware, like scopeOf)"}},"required":["query"]}"#,
        ),
        (
            "sieve_file_api",
            crate::templates::FILE_API_DESC,
            r#"{"type":"object","properties":{"file":{"type":"string","description":"repo-relative path (or unique basename) of the file"}},"required":["file"]}"#,
        ),
        (
            "sieve_check_freshness",
            "Report whether the committed graph is in sync with the code (drift check).",
            r#"{"type":"object","properties":{}}"#,
        ),
        (
            "sieve_trace_calls",
            "Structural edges for a symbol, over call/reference/import/implements/extends ($0, no LLM). Defaults to direct callers (who depends on it). Set direction:\"out\" for callees (what it calls); set depth>1 (or depth:\"all\" for the full closure) to walk transitively for the full blast radius — every source that breaks if it changes. Run before a multi-file refactor to find ALL affected files.",
            r#"{"type":"object","properties":{"symbol":{"type":"string","description":"bare name, qualified (Class.method), or package-qualified (pkg.Fn); a file path also works"},"direction":{"type":"string","enum":["in","out"],"description":"\"in\" (default) = callers/dependents; \"out\" = callees/dependencies"},"depth":{"description":"transitive walk depth for blast radius (default 1 = direct edges only); pass \"all\" for the full connected closure — every source that would be affected"},"in":{"type":"string","description":"narrow matches to nodes at or under this repo-relative path prefix, e.g. server/src"}},"required":["symbol"]}"#,
        ),
        (
            "sieve_find_all",
            crate::templates::FIND_ALL_DESC,
            r#"{"type":"object","properties":{"pattern":{"type":"string","description":"regex pattern (or literal string with fixed: true)"},"in":{"type":"string","description":"narrow to files at or under this repo-relative path prefix, e.g. server/src"},"ignore_case":{"type":"boolean","description":"case-insensitive match"},"fixed":{"type":"boolean","description":"treat pattern as a literal string, not a regex"}},"required":["pattern"]}"#,
        ),
        (
            "sieve_repo_map",
            "Token-budgeted repo orientation — directory clusters, per-directory hubs, and global hotspots computed purely from the wiring graph ($0, no LLM). Use this to get oriented in an unfamiliar repo before diving into files.",
            r#"{"type":"object","properties":{"max_dirs":{"type":"number","description":"max directory entries shown, rest counted into dropped (default 16)"}}}"#,
        ),
    ];
    let mut specs: Vec<ToolSpec> = raw
        .into_iter()
        .map(|(name, description, input_schema)| ToolSpec {
            name: name.to_string(),
            description: description.to_string(),
            input_schema,
        })
        .collect();
    specs.push(ToolSpec {
        name: why_tool_name(),
        description: "Why a symbol exists: the recorded decisions, the reason comments, the pinning tests and the commit history of its span. Every row shows its source. Runs no network call.".to_string(),
        input_schema: r#"{"type":"object","properties":{"symbol":{"type":"string","description":"symbol name, file path, or path:line"},"all":{"type":"boolean","description":"show every row, not the first 10"}},"required":["symbol"]}"#,
    });
    specs
}

/// Maps a legacy alias to its canonical tool name, else returns the name
/// unchanged (`canonicalToolName`, `mcp-server.md` section 3).
pub fn canonical_tool_name(name: &str) -> String {
    let p = product();
    if name == p.tool("ask") {
        p.tool("find_code")
    } else if name == p.tool("grep") {
        p.tool("find_all")
    } else if name == p.tool("callers") {
        p.tool("trace_calls")
    } else if name == p.tool("skeleton") {
        p.tool("file_api")
    } else if name == p.tool("map") {
        p.tool("repo_map")
    } else if name == p.tool("check") {
        p.tool("check_freshness")
    } else {
        name.to_string()
    }
}

/// The boot `instructions` body, with no upkeep prefix
/// (`mcp-server.md` section 2, `mcpInstructions()`).
pub fn instructions() -> String {
    instructions_with_upkeep(&[])
}

/// `instructions`, with the boot upkeep lines first when there are any
///: the lines joined by a newline, then a blank line.
pub fn instructions_with_upkeep(upkeep: &[String]) -> String {
    let body = instructions_body();
    if upkeep.is_empty() {
        body
    } else {
        format!("{}\n\n{body}", upkeep.join("\n"))
    }
}

fn instructions_body() -> String {
    crate::templates::INSTRUCTIONS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_tool_name_maps_every_alias() {
        assert_eq!(canonical_tool_name("sieve_ask"), "sieve_find_code");
        assert_eq!(canonical_tool_name("sieve_grep"), "sieve_find_all");
        assert_eq!(canonical_tool_name("sieve_callers"), "sieve_trace_calls");
        assert_eq!(canonical_tool_name("sieve_skeleton"), "sieve_file_api");
        assert_eq!(canonical_tool_name("sieve_map"), "sieve_repo_map");
        assert_eq!(canonical_tool_name("sieve_check"), "sieve_check_freshness");
    }

    #[test]
    fn canonical_tool_name_keeps_an_unknown_name() {
        assert_eq!(canonical_tool_name("sieve_nope"), "sieve_nope");
    }

    #[test]
    fn tool_specs_has_six_entries_in_order_then_why() {
        let specs = tool_specs();
        let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
        assert_eq!(&names[..6], tool_names().as_slice());
        assert_eq!(names[6], why_tool_name());
        assert_eq!(names.len(), 7);
    }

    /// The MCP instructions hold no savings text under either product.
    #[test]
    fn test_savings_mcp_instructions_hold_no_tally() {
        let text = instructions();
        assert!(!text.contains('\u{1f331}') && !text.contains("tokens saved"));
        assert!(text.starts_with("This repo is indexed by Sieve"));
    }

    #[test]
    fn every_schema_is_valid_json() {
        for spec in tool_specs() {
            let parsed: serde_json::Value = serde_json::from_str(spec.input_schema)
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            assert!(parsed.is_object());
        }
    }
}
