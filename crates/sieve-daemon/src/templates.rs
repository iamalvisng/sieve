//! The texts the MCP server gives to a coding agent: the server
//! instructions and the descriptions of the two most used tools. A private
//! script checks these texts for 8-word runs that an older text shares.

/// The server instructions, with no upkeep lines.
pub const INSTRUCTIONS: &str = r#"This repo is indexed by Sieve. Use these tools for code search and code reading, before Grep, Glob, grep, cat or Read. Each result gives exact file:line, groups hits by symbol, and uses fewer tokens.

If the tool schemas are not loaded yet, load all of them with one ToolSearch call: "select:mcp__sieve__sieve_why,mcp__sieve__sieve_repo_map,mcp__sieve__sieve_find_all,mcp__sieve__sieve_file_api,mcp__sieve__sieve_find_code,mcp__sieve__sieve_trace_calls". One call per tool wastes time.

- sieve_find_code: where is X, how does X work. Ranked hits with code inline.
- sieve_find_all: every use of X. Use it in place of Grep. Set fixed:true for a literal.
- sieve_trace_calls: what calls X, what X calls. Set depth 2 to see what breaks if X changes.
- sieve_file_api: what is in one file, as signatures. Call it before you Read a whole code file.
- sieve_repo_map: overview of an unknown repo.
- sieve_why: why code exists. Give a symbol, a file, or path:line. It returns the recorded decisions, reason comments, tests and history.

The index refreshes before each query. Results include edits you have not committed."#;

/// The description of `sieve_find_all`.
pub const FIND_ALL_DESC: &str = "Find every match of a pattern in indexed code. Use it in place of Grep for code search. Hits are grouped by enclosing symbol and ranked by caller count. Set fixed:true for a literal string.";

/// The description of `sieve_file_api`.
pub const FILE_API_DESC: &str = "List every definition in one file with its signature and line span. Call it before you Read a whole code file.";
