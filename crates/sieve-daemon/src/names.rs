//! The MCP server identity: the server name, the six tool names, their
//! descriptions and schemas, the legacy aliases, and the boot `instructions`
//! text.
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
        product().tool("query"),
        product().tool("symbols"),
        product().tool("check"),
        product().tool("callers"),
        product().tool("find"),
        product().tool("map"),
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
            "sieve_query",
            "Query the repo context graph in plain words. Returns ranked symbols with exact file:line spans and the relevant source inlined — usually the full answer, no file reads needed.",
            r#"{"type":"object","properties":{"query":{"type":"string","description":"what you want to understand, in plain words"},"limit":{"type":"number","description":"max results (default 5)"},"full":{"type":"boolean","description":"inline whole definition spans instead of the default ≤8-line crux excerpts"},"in":{"type":"string","description":"narrow to nodes under this path prefix, filtered before scoring (segment-aware, like scopeOf)"}},"required":["query"]}"#,
        ),
        (
            "sieve_symbols",
            crate::templates::FILE_API_DESC,
            r#"{"type":"object","properties":{"file":{"type":"string","description":"repo-relative path (or unique basename) of the file"}},"required":["file"]}"#,
        ),
        (
            "sieve_check",
            "Report whether the committed graph is in sync with the code (drift check).",
            r#"{"type":"object","properties":{}}"#,
        ),
        (
            "sieve_callers",
            "Structural links for a symbol, over call/reference/import/implements/extends ($0, no LLM). Defaults to direct callers (who depends on it). Set direction:\"out\" for callees (what it calls); set depth>1 (or depth:\"all\" for the full closure) to walk transitively for the full blast radius — every source that breaks if it changes. Run before a multi-file refactor to find ALL affected files.",
            r#"{"type":"object","properties":{"symbol":{"type":"string","description":"bare name, qualified (Class.method), or package-qualified (pkg.Fn); a file path also works"},"direction":{"type":"string","enum":["in","out"],"description":"\"in\" (default) = callers/dependents; \"out\" = callees/dependencies"},"depth":{"description":"transitive walk depth for blast radius (default 1 = direct edges only); pass \"all\" for the full connected closure — every source that would be affected"},"in":{"type":"string","description":"narrow matches to nodes at or under this repo-relative path prefix, e.g. server/src"}},"required":["symbol"]}"#,
        ),
        (
            "sieve_find",
            crate::templates::FIND_ALL_DESC,
            r#"{"type":"object","properties":{"pattern":{"type":"string","description":"regex pattern (or literal string with fixed: true)"},"in":{"type":"string","description":"narrow to files at or under this repo-relative path prefix, e.g. server/src"},"ignore_case":{"type":"boolean","description":"case-insensitive match"},"fixed":{"type":"boolean","description":"treat pattern as a literal string, not a regex"}},"required":["pattern"]}"#,
        ),
        (
            "sieve_map",
            "Token-budgeted repo orientation — directory clusters, per-directory hubs, and global hotspots computed purely from the symbol graph ($0, no LLM). Use this to get oriented in an unfamiliar repo before diving into files.",
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
    specs.push(ToolSpec {
        name: product().tool("rename"),
        description: crate::templates::RENAME_DESC.to_string(),
        input_schema: r#"{"type":"object","properties":{"symbol":{"type":"string","description":"bare name, node id path#name, or path:line"},"new_name":{"type":"string","description":"the new name"},"write":{"type":"boolean","description":"true changes files; needs expect (default false = dry run)"},"expect":{"type":"string","description":"with write:true, the plan_id from a dry run"},"in":{"type":"string","description":"narrow symbol search to this path prefix"}},"required":["symbol","new_name"]}"#,
    });
    specs
}

/// The hidden old tool names and the tool each one now calls.
const OLD_TOOL_NAMES: [(&str, &str); 9] = [
    ("ask", "query"),
    ("find_code", "query"),
    ("grep", "find"),
    ("find_all", "find"),
    ("trace_calls", "callers"),
    ("skeleton", "symbols"),
    ("file_api", "symbols"),
    ("repo_map", "map"),
    ("check_freshness", "check"),
];

/// Maps an old tool name to its current tool name, else returns the name
/// unchanged.
pub fn canonical_tool_name(name: &str) -> String {
    let p = product();
    OLD_TOOL_NAMES
        .iter()
        .find(|(old, _)| name == p.tool(old))
        .map_or_else(|| name.to_string(), |(_, new)| p.tool(new))
}

/// The boot `instructions` body, with no upkeep prefix
///.
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
        for (old, new) in [
            ("sieve_ask", "sieve_query"),
            ("sieve_find_code", "sieve_query"),
            ("sieve_grep", "sieve_find"),
            ("sieve_find_all", "sieve_find"),
            ("sieve_trace_calls", "sieve_callers"),
            ("sieve_skeleton", "sieve_symbols"),
            ("sieve_file_api", "sieve_symbols"),
            ("sieve_repo_map", "sieve_map"),
            ("sieve_check_freshness", "sieve_check"),
        ] {
            assert_eq!(canonical_tool_name(old), new, "{old}");
        }
    }

    #[test]
    fn tool_list_shows_only_the_new_names() {
        let names: Vec<String> = tool_specs().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "sieve_query",
                "sieve_symbols",
                "sieve_check",
                "sieve_callers",
                "sieve_find",
                "sieve_map",
                "sieve_why",
                "sieve_rename"
            ]
        );
    }

    #[test]
    fn canonical_tool_name_keeps_an_unknown_name() {
        assert_eq!(canonical_tool_name("sieve_nope"), "sieve_nope");
    }

    #[test]
    fn tool_specs_has_six_entries_in_order_then_why_and_rename() {
        let specs = tool_specs();
        let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
        assert_eq!(&names[..6], tool_names().as_slice());
        assert_eq!(names[6], why_tool_name());
        assert_eq!(names[7], product().tool("rename"));
        assert_eq!(names.len(), 8);
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
