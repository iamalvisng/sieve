//! The literal --help text sieve prints, byte for byte matched to
//! the recorded Commander-rendered help (P1-01). The usage line
//! names the program as sieve, so this text keeps that
//! name too.

/// The byte-for-byte help text (P1-01). Route every print
/// site through [`help_text`], never this constant directly, so the
/// active product's name replaces `sieve` (P2 rename).
pub const HELP_TEXT: &str = r#"Usage: sieve [options] [command]

Index a repo into a symbol graph with call links, for coding agents.

Options:
  -v, --version                     output the version number
  --dir <path>                      context graph directory (default:
                                    <repo>/sieve)
  -h, --help                        display help for command

Commands:
  telemetry [action]                Show that Sieve sends no usage data. The
                                    command records nothing.
  version                           Print the installed version
  build [options] [dir]             Build sieve/ from your code — symbol graph +
                                    per-file pages ($0, no key).
  ask [options] <query> [dir]       Query the sieve/ graph — returns ranked
                                    symbols + exact file:line, routed to prose or
                                    code ($0, no key)
  skeleton [options] <file> [dir]   Signatures-only view of one file from the
                                    symbol graph — the cheapest way to see a
                                    file's API surface
  check [options] [dir]             Fail if sieve/ is stale relative to the code
                                    (for CI)
  stats [options] [dir]             Show this agent session's sieve-vs-source
                                    usage mix and tokens saved
  viz [options] [dir]               Serve an interactive visualization of the
                                    context graph (and graph.json when present)
  mcp [dir]                         Serve the graph over MCP (stdio) — exposes
                                    sieve_find_code, sieve_trace_calls,
                                    sieve_find_all, sieve_file_api,
                                    sieve_repo_map, sieve_why and
                                    sieve_check_freshness as tools
  callers [options] <symbol> [dir]  Who calls/references a symbol ($0, no LLM).
                                    --direction out gives callees (what it
                                    calls); --depth N (or all) walks
                                    transitively for full blast radius
  blast [options] [dir]             Blast radius of a diff: what depends on the
                                    lines this change touched ($0, no LLM).
                                    Built for CI — `--format markdown` is a PR
                                    comment with a Mermaid diagram.
  grep [options] <pattern> [dir]    Regex search over indexed files, hits
                                    grouped by enclosing symbol and ranked by
                                    coupling ($0, no LLM)
  map [options] [dir]               Token-budgeted repo orientation — directory
                                    clusters, per-directory hubs, and global
                                    hotspots from the symbol graph ($0, no LLM)
  why [options] [symbol] [dir]      Show the decision entries that explain a
                                    symbol
  init [options] [dir]              Wire Sieve into the AI coding agents used
                                    with this repo (instruction files + MCP
                                    server; full hooks + statusline + MCP for
                                    Claude Code)
  uninstall [options] [dir]         Remove every file and config entry sieve has
                                    written to this repo (the inverse of init)
  help [command]                    display help for command
"#;

/// The `--help` text: every
/// print site calls this, never [`HELP_TEXT`] directly.
pub fn help_text() -> String {
    HELP_TEXT.to_string()
}

/// The recorded `<cmd> --help` text for each command Sieve shares with the
/// recorded help (P1-63), byte for byte as commander prints it. A literal table,
/// like [`HELP_TEXT`]: it is the least code and the goldens catch drift.
/// Sieve has no `upgrade`, `trail` or `claude-md`.
/// Sieve-only commands are not here; clap prints their help.
pub const SUB_HELP: &[(&str, &str)] = &[
    (
        "telemetry",
        r#"Usage: sieve telemetry [options] [action]

Show that Sieve sends no usage data. The command records nothing.

Arguments:
  action      status (default) | enable | disable | debug (default: "status")

Options:
  -h, --help  display help for command
"#,
    ),
    (
        "version",
        r#"Usage: sieve version [options]

Print the installed version

Options:
  -h, --help  display help for command
"#,
    ),
    (
        "build",
        r#"Usage: sieve build [options] [dir]

Build sieve/ from your code — symbol graph + per-file pages ($0, no key).

Arguments:
  dir                         repository root (default: ".")

Options:
  -e, --extensions <exts...>  code extensions to include (e.g. ".ts" ".py"); an
                              extension with no parser is ignored with a warning
                              that lists the supported set
  --no-reuse                  re-parse every file instead of replaying unchanged
                              ones from the extraction cache
  --lsp                       add compiler-grade call links via a language
                              server if one is installed (opt-in, slower; e.g.
                              rust-analyzer, clangd)
  --follow-submodules         include initialized Git submodules recursively;
                              persisted for later builds and automatic refreshes
  --no-follow-submodules      exclude Git submodules; persisted for later builds
                              and automatic refreshes (default)
  --follow-nested-repos       include nested Git clones the index does not track
                              (a multi-repo manifest checkout, or any repo
                              cloned into the tree) as ONE graph, so imports
                              across them resolve; persisted for later builds
                              and automatic refreshes
  --no-follow-nested-repos    exclude untracked nested Git clones; persisted for
                              later builds and automatic refreshes (default)
  --include-dir <name>        override SKIP_DIRS for this repo's walks —
                              repeatable (e.g. --include-dir build --include-dir
                              tools); persisted, so a later build (and the
                              hooks/refresh path) include it without the flag;
                              dot-dirs are never overridable (default: [])
  --only-dir <path>           only index files under this repo-relative path —
                              repeatable; the file walk honors it. Recorded in
                              the graph fingerprint so a later build (and the
                              hooks/refresh path) walks the same set; everything
                              outside the list is skipped (default: [])
  --no-gitignore              skip writing sieve/ into .gitignore (same as
                              SIEVE_NO_GITIGNORE=1)
  --no-ignore                 skip writing .ignore for ripgrep re-admit (same as
                              SIEVE_NO_IGNORE=1)
  -h, --help                  display help for command
"#,
    ),
    (
        "ask",
        r#"Usage: sieve ask [options] <query> [dir]

Query the sieve/ graph — returns ranked symbols + exact file:line, routed to prose
or code ($0, no key)

Arguments:
  query            what you want to understand, in plain words
  dir              repository root (default: nearest ancestor with a sieve/
                   index)

Options:
  -n, --limit <n>  max results (default: "8")
  --source         inline the source at each file:line hit (retriever mode — the
                   pack IS the answer, no need to re-open files)
  --full           with --source: inline whole definition spans instead of the
                   default ≤8-line crux excerpts
  --in <path>      narrow to symbols under this path prefix, filtered before
                   scoring (segment-aware, like scopeOf)
  --json           output the result as JSON
  --no-graph-rank  rank by lexical relevance only, without the
                   graph-connectivity re-rank (ablation/eval)
  --no-refresh     skip the freshness check — answer from the graph as-is
  -h, --help       display help for command
"#,
    ),
    (
        "skeleton",
        r#"Usage: sieve skeleton [options] <file> [dir]

Signatures-only view of one file from the symbol graph — the cheapest way to see
a file's API surface

Arguments:
  file          repo-relative path (or unique basename) of the file
  dir           repository root (default: nearest ancestor with a sieve/ index)

Options:
  --json        output the result as JSON
  --no-refresh  skip the freshness check — answer from the graph as-is
  -h, --help    display help for command
"#,
    ),
    (
        "check",
        r#"Usage: sieve check [options] [dir]

Fail if sieve/ is stale relative to the code (for CI)

Arguments:
  dir                         repository root (default: nearest ancestor with a
                              sieve/ index)

Options:
  -e, --extensions <exts...>  code extensions to include
  --json                      output the drift as JSON
  -h, --help                  display help for command
"#,
    ),
    (
        "stats",
        r#"Usage: sieve stats [options] [dir]

Show this agent session's sieve-vs-source usage mix and tokens saved

Arguments:
  dir         repository root (default: nearest ancestor with a sieve/ index)

Options:
  --json      output the session stats as JSON
  -h, --help  display help for command
"#,
    ),
    (
        "viz",
        r#"Usage: sieve viz [options] [dir]

Serve an interactive visualization of the context graph (and graph.json when
present)

Arguments:
  dir                repository root (default: nearest ancestor with a sieve/
                     index)

Options:
  -p, --port <port>  port to serve on (default: "4400")
  --no-open          don't open the browser
  --export <dir>     write one self-contained index.html instead of serving (for
                     CI, GitHub Pages, or a build artifact)
  --title <text>     subtitle shown beside the repo name in an exported page
                     (e.g. "PR #151")
  --tabs <list>      tabs the exported page offers, comma separated:
                     context,code,outline (default: all three)
  -h, --help         display help for command
"#,
    ),
    (
        "mcp",
        r#"Usage: sieve mcp [options] [dir]

Serve the graph over MCP (stdio) — exposes sieve_find_code, sieve_trace_calls,
sieve_find_all, sieve_file_api, sieve_repo_map, sieve_why and
sieve_check_freshness as tools

Arguments:
  dir         repository root (default: nearest ancestor with a sieve/ index)

Options:
  -h, --help  display help for command
"#,
    ),
    (
        "callers",
        r#"Usage: sieve callers [options] <symbol> [dir]

Who calls/references a symbol ($0, no LLM). --direction out gives callees (what
it calls); --depth N (or all) walks transitively for full blast radius

Arguments:
  symbol                bare name, qualified (Class.method), or
                        package-qualified (pkg.Fn)
  dir                   repository root (default: nearest ancestor with a sieve/
                        index)

Options:
  --direction <in|out>  link direction: "in" = callers (default), "out" =
                        callees
  -d, --depth <n>       walk transitively up to N hops for blast radius, or
                        "all" for the full connected closure (default 1)
  --in <path>           narrow matches to symbols at or under this path prefix
  --json                output as JSON
  --no-refresh          skip the freshness check — answer from the graph as-is
  -h, --help            display help for command
"#,
    ),
    (
        "blast",
        r#"Usage: sieve blast [options] [dir]

Blast radius of a diff: what depends on the lines this change touched ($0, no
LLM). Built for CI — `--format markdown` is a PR comment with a Mermaid diagram.

Arguments:
  dir                   repository root (default: nearest ancestor with a sieve/
                        index)

Options:
  --base <ref>          diff against this ref's merge base with HEAD (e.g.
                        origin/main); default: the working tree vs HEAD
  -d, --depth <n>       hops to walk over incoming links, or "all" for the full
                        closure (default 2)
  --format <fmt>        text (default) | markdown | mermaid | json
  --export-viz <dir>    also write the interactive page for this radius (one
                        self-contained index.html — for CI, GitHub Pages, or an
                        artifact)
  --title <text>        subtitle beside the repo name on the exported page (e.g.
                        "PR #171")
  --no-owners           do not suggest who to tag (by default, git history names
                        the people behind each affected area)
  --pr-author <who...>  GitHub login, git name or email of the PR author, so
                        they are left out of their own suggestions
  --no-refresh          skip the freshness check — answer from the graph as-is
  -h, --help            display help for command
"#,
    ),
    (
        "grep",
        r#"Usage: sieve grep [options] <pattern> [dir]

Regex search over indexed files, hits grouped by enclosing symbol and ranked by
coupling ($0, no LLM)

Arguments:
  pattern            regex pattern (or literal string with --fixed)
  dir                repository root (default: nearest ancestor with a sieve/
                     index)

Options:
  -i, --ignore-case  case-insensitive match
  --fixed            treat pattern as a literal string, not a regex
  --in <path>        narrow to files at or under this path prefix
  --json             output as JSON
  --no-refresh       skip the freshness check — answer from the graph as-is
  -h, --help         display help for command
"#,
    ),
    (
        "map",
        r#"Usage: sieve map [options] [dir]

Token-budgeted repo orientation — directory clusters, per-directory hubs, and
global hotspots from the symbol graph ($0, no LLM)

Arguments:
  dir             repository root (default: nearest ancestor with a sieve/
                  index)

Options:
  --max-dirs <n>  max directory entries shown, rest counted into dropped
                  (default 16)
  --json          output as JSON
  --no-refresh    skip the freshness check — answer from the graph as-is
  -h, --help      display help for command
"#,
    ),
    (
        "init",
        r#"Usage: sieve init [options] [dir]

Wire Sieve into the AI coding agents used with this repo (instruction files +
MCP server; full hooks + statusline + MCP for Claude Code)

Arguments:
  dir                target repo directory (default: ".")

Options:
  --no-build         skip building the graph (wire files only)
  --agents <ids...>  only these agents (agents, adal, cursor, gemini, grok,
                     hermes, antigravity, copilot, kiro, windsurf, claude)
  --all-agents       write instruction files for every known agent, detected or
                     not
  --no-agents        Claude Code only; skip other agents
  --list-agents      list known agent ids and exit
  --no-mcp           skip MCP server registration for other agents
  --no-hooks         skip hook installation for other agents
  --no-statusline    skip writing Claude Code statusLine (keep a user-defined
                     one)
  --dry-run          print every file init would touch, then exit without
                     writing
  -y, --yes          skip the picker and wire every detected agent (the pre-0.8
                     default)
  --no-global        skip writes outside this repo (the ~/.codex/ config +
                     hooks)
  --verbose          print every file written, the graph build's own output and
                     the closing banner
  -h, --help         display help for command
"#,
    ),
    (
        "uninstall",
        r#"Usage: sieve uninstall [options] [dir]

Remove every file and config entry sieve has written to this repo (the inverse
of init)

Arguments:
  dir           target repo directory (default: ".")

Options:
  -y, --yes     actually remove (without this, prints what it would remove and
                exits)
  --keep-cache  keep sieve/ and the .gitignore entries — remove agent files only
  --no-global   leave out-of-repo files alone (~/.codex, ~/.gemini)
  -h, --help    display help for command
"#,
    ),
    (
        "_brain-refresh",
        r#"Usage: sieve _brain-refresh [options] [dir]

internal: re-pull the attached brain's rules and rewrite the agent files

Arguments:
  dir         target repo directory (default: ".")

Options:
  -h, --help  display help for command
"#,
    ),
    (
        "_update-check",
        r#"Usage: sieve _update-check [options]

internal: refresh the cached latest-version answer

Options:
  -h, --help  display help for command
"#,
    ),
    (
        "_telemetry-flush",
        r#"Usage: sieve _telemetry-flush [options]

internal: POST the queued anonymous usage events

Options:
  -h, --help  display help for command
"#,
    ),
];

/// The branded commander help for one subcommand, or `None` when there
/// is no text for it (the caller then lets clap print its own help).
pub fn sub_help_text(name: &str) -> Option<String> {
    let (_, text) = SUB_HELP.iter().find(|(n, _)| *n == name)?;
    // The base `init` text does not list `--mod`; add it here.
    if name == "init" {
        return Some(text.replace(
            "  -h, --help ",
            "  --mod              install the sieve-pane Claude Code mod into the project\n  -h, --help ",
        ));
    }
    Some(text.to_string())
}
