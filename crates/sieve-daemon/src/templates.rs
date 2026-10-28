//! The texts the MCP server gives to a coding agent: the server
//! instructions and the descriptions of the two most used tools. A private
//! script checks these texts for 8-word runs that an older text shares.

/// The server instructions, with no upkeep lines.
pub const INSTRUCTIONS: &str = r#"This repo is indexed by Sieve. Use these tools for code search and code reading, before Grep, Glob, grep, cat or Read. Each result gives exact file:line, groups hits by symbol, and uses fewer tokens.

If the tool schemas are not loaded yet, load all of them with one ToolSearch call: "select:mcp__sieve__sieve_why,mcp__sieve__sieve_map,mcp__sieve__sieve_find,mcp__sieve__sieve_symbols,mcp__sieve__sieve_query,mcp__sieve__sieve_callers". One call per tool wastes time.

- sieve_query: where is X, how does X work. Ranked hits with code inline.
- sieve_find: every use of X. Use it in place of Grep. Set fixed:true for a literal.
- sieve_callers: what calls X, what X calls. Set depth 2 to see what breaks if X changes.
- sieve_symbols: what is in one file, as signatures. Call it before you Read a whole code file.
- sieve_map: overview of an unknown repo.
- sieve_why: why code exists. Give a symbol, a file, or path:line. It returns the recorded decisions, reason comments, tests and history.
- sieve_rename: rename one TS or JS function across files. A call without write is a dry run. write:true changes files and needs the plan_id of a dry run in expect.

The index refreshes before each query. Results include edits you have not committed."#;

/// The description of `sieve_find`.
pub const FIND_ALL_DESC: &str = "Find every match of a pattern in indexed code. Use it in place of Grep for code search. Hits are grouped by enclosing symbol and ranked by caller count. Set fixed:true for a literal string.";

/// The description of `sieve_symbols`.
pub const FILE_API_DESC: &str = "List every definition in one file with its signature and line span. Call it before you Read a whole code file.";

/// The description of `sieve_rename`.
pub const RENAME_DESC: &str = "Rename one symbol across all files from proven sites. The default is a dry run that returns the plan, the plan_id and any refusal reasons. write:true changes files and needs expect set to the plan_id of a dry run.";
