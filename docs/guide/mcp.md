# MCP server

`sieve mcp [dir]` serves the index over MCP on stdio. `sieve init` registers
it for your agents. The registered command is `sieve` with the argument `mcp`.

## Register by hand

Add this to your agent's MCP config:

```json
{ "mcpServers": { "sieve": { "command": "sieve", "args": ["mcp"] } } }
```

Codex and Grok use TOML:

```toml
[mcp_servers.sieve]
command = "sieve"
args = ["mcp"]
```

Restart the agent after you change its MCP config.

## Tools

The server has seven tools.

| Tool | Inputs | Use |
|---|---|---|
| `sieve_find_code` | `query` (required), `limit`, `full`, `in` | Ask in plain words. Like `sieve ask`. |
| `sieve_find_all` | `pattern` (required), `in`, `ignore_case`, `fixed` | Every match of a pattern. Like `sieve grep`. |
| `sieve_trace_calls` | `symbol` (required), `direction`, `depth`, `in` | Callers or callees. Like `sieve callers`. |
| `sieve_file_api` | `file` (required) | Signatures of one file. Like `sieve skeleton`. |
| `sieve_repo_map` | `max_dirs` | Orientation. Like `sieve map`. |
| `sieve_check_freshness` | none | Reports drift between the index and the code. |
| `sieve_why` | `symbol` (required), `all` | The decision records for a symbol. |

Notes:

- `direction` is `in` (default) or `out`.
- `depth` is a number, or `all` for the full closure.
- `in` is a repo-relative path prefix.
- `limit` defaults to 5.
- `max_dirs` defaults to 16.
- `symbol` for `sieve_why` is a name, a file path or `path:line`.

## Old names

The old names `sieve_ask`, `sieve_grep`, `sieve_callers`, `sieve_skeleton`,
`sieve_map` and `sieve_check` map to the tools above.
