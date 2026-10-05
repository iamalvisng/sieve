//! The host registry, the write and merge logic `sieve init` and
//! `sieve uninstall` share, and the render of the shared instruction body
//! and the SKILL.md template (the `hosts-hooks.md` note sections 1, 2, 5, 7).
//! The texts live in `templates.rs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sieve_core::product::product;

use crate::names;
use crate::ojson::{OJson, OMap};

/// How a host's instruction file is written (section 1.2, 1.4).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Sieve overwrites the whole file on any content change.
    Owned,
    /// Sieve upserts a fenced block inside a file the user may also own.
    Section,
}

/// How a host wraps the shared [`INSTRUCTION_BODY`] (section 1.5).
#[derive(Clone, Copy)]
pub enum Wrap {
    /// The plain body, fenced by the section markers.
    Section,
    /// YAML frontmatter for a Cursor rule, then the body.
    CursorRule,
    /// YAML frontmatter for a Kiro steering file, then the body.
    KiroSteering,
    /// The body plus a trailing newline, no frontmatter.
    WindsurfRule,
    /// The shared SKILL.md template, verbatim.
    Skill,
}

/// One host `sieve init` can wire into, in the registry order the golden
/// stderr report pins (`hosts-hooks.md` section 1.2).
pub struct HostSpec {
    pub id: &'static str,
    pub kind: Kind,
    pub rel_path: String,
    pub wrap: Wrap,
}

/// The host registry, in registry order. Every path under a product-named
/// skill or rule directory is built at runtime, from the active product's
/// name (P2 rename); every other path stays a plain literal.
pub fn hosts() -> Vec<HostSpec> {
    let name = product().name;
    vec![
        HostSpec {
            id: "agents",
            kind: Kind::Section,
            rel_path: "AGENTS.md".to_string(),
            wrap: Wrap::Section,
        },
        HostSpec {
            id: "adal",
            kind: Kind::Owned,
            rel_path: format!(".adal/skills/{name}/SKILL.md"),
            wrap: Wrap::Skill,
        },
        HostSpec {
            id: "cursor",
            kind: Kind::Owned,
            rel_path: format!(".cursor/rules/{name}.mdc"),
            wrap: Wrap::CursorRule,
        },
        HostSpec {
            id: "gemini",
            kind: Kind::Section,
            rel_path: "GEMINI.md".to_string(),
            wrap: Wrap::Section,
        },
        HostSpec {
            id: "grok",
            kind: Kind::Owned,
            rel_path: format!(".grok/skills/{name}/SKILL.md"),
            wrap: Wrap::Skill,
        },
        HostSpec {
            id: "hermes",
            kind: Kind::Section,
            rel_path: "AGENTS.md".to_string(),
            wrap: Wrap::Section,
        },
        HostSpec {
            id: "antigravity",
            kind: Kind::Section,
            rel_path: "AGENTS.md".to_string(),
            wrap: Wrap::Section,
        },
        HostSpec {
            id: "copilot",
            kind: Kind::Section,
            rel_path: ".github/copilot-instructions.md".to_string(),
            wrap: Wrap::Section,
        },
        HostSpec {
            id: "kiro",
            kind: Kind::Owned,
            rel_path: format!(".kiro/steering/{name}.md"),
            wrap: Wrap::KiroSteering,
        },
        HostSpec {
            id: "windsurf",
            kind: Kind::Owned,
            rel_path: format!(".windsurf/rules/{name}.md"),
            wrap: Wrap::WindsurfRule,
        },
    ]
}

/// One host's local MCP config target, in the order the golden stderr
/// report pins. Each target holds a `sieve` entry keyed the same way as
/// `.mcp.json` and `.cursor/mcp.json` — a `command`/`args` pair — except
/// `grok`, whose config is TOML, handled separately.
pub struct McpTarget {
    pub id: &'static str,
    pub rel_path: &'static str,
    pub under_home: bool,
}

/// The per-host MCP config targets that get a JSON `mcpServers.sieve`
/// entry (`grok`'s TOML target is separate, see [`grok_toml_block`]).
pub fn json_mcp_targets() -> Vec<McpTarget> {
    vec![
        McpTarget {
            id: "cursor",
            rel_path: ".cursor/mcp.json",
            under_home: false,
        },
        McpTarget {
            id: "gemini",
            rel_path: ".gemini/settings.json",
            under_home: false,
        },
        McpTarget {
            id: "antigravity",
            rel_path: ".gemini/config/mcp_config.json",
            under_home: true,
        },
        McpTarget {
            id: "kiro",
            rel_path: ".kiro/settings/mcp.json",
            under_home: false,
        },
    ]
}

/// Codex's MCP config, `~/.codex/config.toml`. Sieve writes it only when
/// the Codex CLI is installed, so `~/.codex` must be a directory
/// (P4-51).
pub fn codex_mcp_path(home: &Path) -> Option<PathBuf> {
    let dir = home.join(".codex");
    dir.is_dir().then(|| dir.join("config.toml"))
}

/// The repo's `opencode.json`. Sieve writes it only when opencode is
/// installed, so `~/.config/opencode` must be a directory
/// (P4-51).
pub fn opencode_mcp_path(root: &Path, home: &Path) -> Option<PathBuf> {
    home.join(".config")
        .join("opencode")
        .is_dir()
        .then(|| root.join("opencode.json"))
}

/// The canonical instruction body, with no trailing newline. `section`
/// hosts fence this verbatim; `owned` hosts wrap it.
pub fn instruction_body() -> String {
    crate::templates::INSTRUCTION_BODY.to_string()
}

/// The shared SKILL.md template, with its own trailing newline included.
pub fn skill_template() -> String {
    crate::templates::SKILL_TEMPLATE.to_string()
}

/// Renders an `owned` host's full file content for its [`Wrap`].
pub fn render_owned(wrap: Wrap) -> String {
    match wrap {
        Wrap::CursorRule => format!(
            "{}\n{}\n",
            "---\ndescription: Use the sieve index for code search and code reading before Grep or Read\nalwaysApply: true\n---",
            instruction_body()
        ),
        Wrap::KiroSteering => format!(
            "---\ninclusion: always\n---\n{}\n",
            instruction_body()
        ),
        Wrap::WindsurfRule => format!("{}\n", instruction_body()),
        Wrap::Skill => skill_template(),
        Wrap::Section => panic!("render_owned called with Wrap::Section"),
    }
}

/// The fenced section's start marker (section 1.4), branded.
pub fn section_start() -> String {
    format!("<!-- {}:start -->", product().name)
}
/// The fenced section's end marker (section 1.4), branded.
pub fn section_end() -> String {
    format!("<!-- {}:end -->", product().name)
}

/// The outcome of writing or merging one target file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAction {
    Created,
    Replaced,
    /// A merge changed an existing file.
    Updated,
    /// A section was added to an existing file.
    Appended,
    Unchanged,
    /// The existing file could not be parsed; nothing was written.
    SkippedUnparseable,
}

impl WriteAction {
    /// The report word the golden stderr lines use for this action.
    pub fn word(self) -> &'static str {
        match self {
            WriteAction::Created => "created",
            WriteAction::Replaced => "replaced",
            WriteAction::Updated => "updated",
            WriteAction::Appended => "appended",
            WriteAction::Unchanged => "unchanged",
            WriteAction::SkippedUnparseable => "skipped-unparseable",
        }
    }
}

/// Writes an `owned` target: overwrites on any content change, otherwise
/// leaves the file untouched (section 1.4, `writeOwned`).
pub fn write_owned(path: &Path, content: &str) -> io::Result<WriteAction> {
    let existed = path.is_file();
    if existed {
        let current = fs::read_to_string(path).unwrap_or_default();
        if current == content {
            return Ok(WriteAction::Unchanged);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(if existed {
        WriteAction::Replaced
    } else {
        WriteAction::Created
    })
}

/// Builds the fenced block `upsertSection` writes or matches: the start
/// marker, the body with trailing whitespace stripped, and the end
/// marker (section 1.4).
fn section_block(body: &str) -> String {
    format!(
        "{}\n{}\n{}",
        section_start(),
        body.trim_end(),
        section_end()
    )
}

/// Upserts the fenced sieve block into a `section` target: creates the
/// file when missing, replaces an existing block in place, appends the
/// block to a markerless file, or does nothing when the block already
/// matches (section 1.4, `upsertSection`).
///
/// ponytail: always appends with a blank-line separator for a markerless
/// non-empty file, rather than sniffing the file's own EOL style; add
/// CRLF/no-blank-line handling if a real foreign file needs it.
pub fn upsert_section(path: &Path, body: &str) -> io::Result<WriteAction> {
    let block = section_block(body);
    if !path.is_file() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, format!("{block}\n"))?;
        return Ok(WriteAction::Created);
    }
    let current = fs::read_to_string(path).unwrap_or_default();
    if let Some((start, end)) = marker_span(&current) {
        let existing_block = &current[start..end];
        if existing_block == block {
            return Ok(WriteAction::Unchanged);
        }
        let new_content = format!("{}{}{}", &current[..start], block, &current[end..]);
        fs::write(path, new_content)?;
        return Ok(WriteAction::Replaced);
    }
    let sep = if current.is_empty() || current.ends_with("\n\n") {
        ""
    } else if current.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let new_content = format!("{current}{sep}{block}\n");
    fs::write(path, new_content)?;
    Ok(WriteAction::Appended)
}

/// Finds the byte span `[start, end)` of an existing fenced block,
/// including both marker lines, or `None` when the markers are absent.
/// JS `String.prototype.trim` whitespace. It holds U+FEFF and not U+0085,
/// which Rust's `char::is_whitespace` has the other way round.
fn js_ws(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(js_ws)
}

fn js_trim_start(s: &str) -> &str {
    s.trim_start_matches(js_ws)
}

fn js_trim_end(s: &str) -> &str {
    s.trim_end_matches(js_ws)
}

/// The `/\n{3,}/g -> "\n\n"`.
fn collapse_blank_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            if run > 2 {
                continue;
            }
        } else {
            run = 0;
        }
        out.push(c);
    }
    out
}

fn marker_span(text: &str) -> Option<(usize, usize)> {
    // Sieve anchors on the whole trimmed line: a
    // marker quoted inside prose is not a fence.
    let start_marker = section_start();
    let end_marker = section_end();
    let mut start_at = None;
    for (at, line) in text_lines(text) {
        match start_at {
            None if js_trim(line) == start_marker => {
                start_at = Some(at + (line.len() - js_trim_start(line).len()));
            }
            Some(s) if js_trim(line) == end_marker => {
                return Some((s, at + js_trim_end(line).len()));
            }
            _ => {}
        }
    }
    None
}

/// Removes a `section` target's fenced block for `sieve uninstall`.
/// Deletes the whole file when nothing but whitespace is left, else
/// rewrites the file without the block. Returns a [`Retract`]; with `apply` false it
/// only reports.
pub fn strip_section(path: &Path, apply: bool) -> io::Result<Retract> {
    if !path.is_file() {
        return Ok(Retract::Absent);
    }
    let current = fs::read_to_string(path)?;
    // A start marker with no end marker is not a block: nothing is removed
    // (a plain rewrite would drop the rest of the file).
    if marker_span(&current).is_none() {
        return Ok(Retract::Absent);
    }
    // Drop the lines of the
    // block, collapse 3+ newlines to 2, drop leading newlines, fold the
    // trailing newlines to one, keep the file's line ending.
    let crlf = current.contains("\r\n");
    let (start, end) = (section_start(), section_end());
    let mut out: Vec<&str> = Vec::new();
    let mut closing: Option<&str> = None;
    let normal = current.replace("\r\n", "\n");
    for line in normal.split('\n') {
        let t = js_trim(line);
        match closing {
            None if t == start => closing = Some(&end),
            None => out.push(line),
            Some(c) if t == c => closing = None,
            Some(_) => {}
        }
    }
    let joined = collapse_blank_runs(&out.join("\n"));
    let mut kept = joined.trim_start_matches('\n').to_string();
    if kept.ends_with('\n') {
        kept = format!("{}\n", kept.trim_end_matches('\n'));
    }
    let text = (!js_trim(&kept).is_empty()).then(|| {
        if crlf {
            kept.replace('\n', "\r\n")
        } else {
            kept
        }
    });
    finish(path, text, apply)
}

/// What a strip did to one file, or would do when `apply` is false:
/// `absent`, `removed`, `deleted`, `skipped-unparseable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retract {
    /// The file or the Sieve content is not there.
    Absent,
    /// Sieve content left; the file stays with the user's content.
    Removed,
    /// Nothing else was in the file, so the file is gone.
    Deleted,
    /// The file is not valid JSON: left untouched.
    Unparseable,
}

impl Retract {
    /// True when the strip changed, or would change, the file.
    pub fn hit(self) -> bool {
        matches!(self, Retract::Removed | Retract::Deleted)
    }
}

/// Ends a strip: rewrites `path` with `text`, or deletes `path` when
/// `text` is `None`. With `apply` false it only reports what would happen.
fn finish(path: &Path, text: Option<String>, apply: bool) -> io::Result<Retract> {
    if apply {
        match &text {
            Some(t) => fs::write(path, t)?,
            None => fs::remove_file(path)?,
        }
    }
    Ok(if text.is_some() {
        Retract::Removed
    } else {
        Retract::Deleted
    })
}

/// The start of a JSON strip: `Err(state)` when the file is missing
/// (`Absent`) or not a JSON object (`Unparseable`), else the parsed root.
fn json_root(path: &Path) -> io::Result<Result<OJson, Retract>> {
    if !path.is_file() {
        return Ok(Err(Retract::Absent));
    }
    Ok(read_json_object(path)?.ok_or(Retract::Unparseable))
}

/// Deletes an `owned` target outright for `sieve uninstall` (section
/// 1.12: "owned" files are always Sieve's own, never a foreign file with
/// Sieve content merged in). Returns a [`Retract`].
pub fn remove_owned(path: &Path, apply: bool) -> io::Result<Retract> {
    if path.is_file() {
        finish(path, None, apply)
    } else {
        Ok(Retract::Absent)
    }
}

/// The `sieve` MCP server entry every `mcpServers.sieve` bucket gets.
pub fn mcp_entry() -> OJson {
    OJson::obj(vec![
        ("command", OJson::s(names::MCP_COMMAND)),
        (
            "args",
            OJson::arr(names::MCP_ARGS.iter().map(|a| OJson::s(*a)).collect()),
        ),
    ])
}

/// Reads `path` as a JSON object, or `None` when the file is missing or
/// not a JSON object. The value keeps the file's key order and its
/// numbers and `null`s as written, so a rewrite changes only what it edits.
fn read_json_object(path: &Path) -> io::Result<Option<OJson>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    match OJson::parse(&text) {
        Some(v @ OJson::Obj(_)) => Ok(Some(v)),
        _ => Ok(None),
    }
}

/// Merges a `sieve` entry into `path`'s `<bucket_key>` object, creating
/// the file, its parent dirs, and the bucket as needed (section 1.8,
/// `mergeJsonKey`). Skips an existing file that fails to parse as a JSON
/// object, reporting [`WriteAction::SkippedUnparseable`].
pub fn merge_mcp_json(path: &Path, bucket_key: &str) -> io::Result<WriteAction> {
    merge_json_entry(path, bucket_key, product().mcp_key(), mcp_entry())
}

/// Merges `entry` as `entry_key` of `path`'s `<bucket_key>` object. Writes
/// nothing when the file is not a JSON object or the bucket has another type.
pub fn merge_json_entry(
    path: &Path,
    bucket_key: &str,
    entry_key: &str,
    entry: OJson,
) -> io::Result<WriteAction> {
    let existed = path.is_file();
    if existed {
        let text = fs::read_to_string(path)?;
        let Ok(serde_json::Value::Object(_)) = serde_json::from_str::<serde_json::Value>(&text)
        else {
            return Ok(WriteAction::SkippedUnparseable);
        };
    }
    let root_oj = read_json_object(path)?;
    let mut root = OMap::from_existing(root_oj.as_ref());

    let bucket_oj = root_oj
        .as_ref()
        .and_then(OJson::as_obj)
        .and_then(|pairs| pairs.iter().find(|(k, _)| k == bucket_key))
        .map(|(_, v)| v.clone());
    // A bucket that is
    // neither an object nor null is the user's value. Write nothing.
    if bucket_oj
        .as_ref()
        .is_some_and(|b| !matches!(b, OJson::Obj(_) | OJson::Null))
    {
        return Ok(WriteAction::SkippedUnparseable);
    }
    let mut bucket = OMap::from_existing(bucket_oj.as_ref());
    bucket.set(entry_key, entry);
    root.set(bucket_key, bucket.into_ojson());

    let new_text = root.into_ojson().to_pretty() + "\n";
    if existed {
        let old_text = fs::read_to_string(path)?;
        if old_text == new_text {
            return Ok(WriteAction::Unchanged);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, new_text)?;
    Ok(if existed {
        WriteAction::Updated
    } else {
        WriteAction::Created
    })
}

/// The `opencode.json` entry.
fn opencode_entry() -> OJson {
    let mut command = vec![OJson::s(names::MCP_COMMAND)];
    command.extend(names::MCP_ARGS.iter().map(|a| OJson::s(*a)));
    OJson::obj(vec![
        ("type", OJson::s("local")),
        ("command", OJson::arr(command)),
        ("enabled", OJson::Bool(true)),
    ])
}

/// Merges Sieve's entry into `opencode.json` under `mcp` (P4-51).
pub fn merge_opencode_json(path: &Path) -> io::Result<WriteAction> {
    merge_json_entry(path, "mcp", product().mcp_key(), opencode_entry())
}

/// Removes the `sieve` entry from `path`'s `<bucket_key>` object for
/// `sieve uninstall`. Deletes the whole file when the bucket and the
/// root object are both left empty. Returns a [`Retract`]; with `apply`
/// false it only reports.
pub fn strip_mcp_json(path: &Path, bucket_key: &str, apply: bool) -> io::Result<Retract> {
    let root_oj = match json_root(path)? {
        Ok(oj) => oj,
        Err(state) => return Ok(state),
    };
    let mut root = OMap::from_existing(Some(&root_oj));
    let bucket_oj = root_oj
        .as_obj()
        .and_then(|pairs| pairs.iter().find(|(k, _)| k == bucket_key))
        .map(|(_, v)| v.clone());
    // No product key in an
    // object bucket means 'absent'. The file stays untouched, even when
    // the bucket is empty.
    let has_key = bucket_oj
        .as_ref()
        .and_then(OJson::as_obj)
        .is_some_and(|pairs| pairs.iter().any(|(k, _)| k == product().mcp_key()));
    if !has_key {
        return Ok(Retract::Absent);
    }
    let mut bucket = OMap::from_existing(bucket_oj.as_ref());
    bucket.remove(product().mcp_key());
    if bucket.is_empty() {
        root.remove(bucket_key);
    } else {
        root.set(bucket_key, bucket.into_ojson());
    }
    let emptied = root.is_empty();
    let after = root.into_ojson();
    // No Sieve key in the file: leave it byte for byte, so `init`'s
    // retract step never rewrites a user's own config (P4-49).
    if after.to_pretty() == root_oj.to_pretty() {
        return Ok(Retract::Absent);
    }
    finish(path, (!emptied).then(|| after.to_pretty() + "\n"), apply)
}

/// The TOML block `grok`'s config gets (there is no JSON `mcpServers`
/// bucket for this host).
pub fn grok_toml_block() -> String {
    format!(
        "[mcp_servers.{}]\ncommand = \"{}\"\nargs = [\"{}\"]\n",
        product().mcp_key(),
        names::MCP_COMMAND,
        names::MCP_ARGS[0]
    )
}

/// Writes or replaces the `[mcp_servers.sieve]` TOML section of `grok`'s
/// and Codex's config.
/// A file that is not UTF-8 text is left unchanged.
///
/// ponytail: table replacement is a plain substring/section splice, not a
/// TOML parse; good enough while grok's config never carries a second
/// `[mcp_servers.*]` table in this repo's fixtures. Add a real TOML
/// parser if that changes.
pub fn merge_mcp_toml(path: &Path) -> io::Result<WriteAction> {
    let block = grok_toml_block();
    let existed = path.is_file();
    let current = if existed {
        match fs::read_to_string(path) {
            Ok(text) if !defines_server_another_way(&text) => text,
            _ => return Ok(WriteAction::SkippedUnparseable),
        }
    } else {
        String::new()
    };
    let new_content = if let Some(span) = toml_section_span(&current) {
        if current[span.0..span.1] == block {
            return Ok(WriteAction::Unchanged);
        }
        format!("{}{}{}", &current[..span.0], block, &current[span.1..])
    } else {
        // Append the section.
        let sep = if current.trim().is_empty() || current.ends_with("\n\n") {
            ""
        } else if current.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let head = if current.trim().is_empty() {
            ""
        } else {
            &current
        };
        format!("{head}{sep}{block}")
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, new_content)?;
    Ok(if existed {
        WriteAction::Updated
    } else {
        WriteAction::Created
    })
}

/// True when the file sets `mcp_servers.<product>` as an inline table or
/// a dotted key. A second `[mcp_servers.<product>]` table would then make
/// the file invalid TOML, so the merge leaves the file unchanged. Conservative:
/// a key named like ours in the
/// root or in `[mcp_servers]` counts, even inside a multi-line string.
fn defines_server_another_way(text: &str) -> bool {
    let key = product().mcp_key();
    let names_key = |s: &str| {
        s.strip_prefix(key)
            .is_some_and(|r| r.starts_with(|c: char| c == '.' || c == '=' || js_ws(c)))
    };
    let mut table = String::new();
    for line in text.split('\n') {
        let l = js_trim(line);
        if l.starts_with('[') {
            table = l
                .trim_matches(|c: char| c == '[' || c == ']' || js_ws(c))
                .to_string();
        } else if table.is_empty() {
            if let Some(rest) = l.strip_prefix("mcp_servers") {
                let rest = js_trim_start(rest);
                if rest
                    .strip_prefix('.')
                    .is_some_and(|r| names_key(js_trim_start(r)))
                    || (rest.starts_with('=') && rest.contains(key))
                {
                    return true;
                }
            }
        } else if table == "mcp_servers" && names_key(l) {
            return true;
        }
    }
    false
}

/// Removes the `[mcp_servers.sieve]` TOML section of `grok`'s or Codex's
/// config for `sieve uninstall`. Deletes the whole file when nothing else is left. Returns
/// a [`Retract`].
pub fn strip_mcp_toml(path: &Path, apply: bool) -> io::Result<Retract> {
    if !path.is_file() {
        return Ok(Retract::Absent);
    }
    let Ok(current) = fs::read_to_string(path) else {
        return Ok(Retract::Absent);
    };
    // Cut the lines from
    // the header to the next `[` line, collapse 3+ newlines to 2, drop
    // leading newlines. Every other byte stays, CRLF and indent too.
    let header = toml_header();
    let lines: Vec<&str> = current.split('\n').collect();
    let Some(start) = lines.iter().position(|l| js_trim(l) == header) else {
        return Ok(Retract::Absent);
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| js_trim_start(l).starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    let rest = collapse_blank_runs(&[&lines[..start], &lines[end..]].concat().join("\n"));
    let rest = rest.trim_start_matches('\n');
    let text = (!js_trim(rest).is_empty()).then(|| {
        if rest.ends_with('\n') {
            rest.to_string()
        } else {
            format!("{rest}\n")
        }
    });
    finish(path, text, apply)
}

/// The `[mcp_servers.sieve]` table header.
fn toml_header() -> String {
    format!("[mcp_servers.{}]", product().mcp_key())
}

/// Finds the byte span of a `[mcp_servers.sieve]` table: from its header
/// line to just before the next top-level `[` header or EOF.
fn toml_section_span(text: &str) -> Option<(usize, usize)> {
    let header = toml_header();
    // The header is a
    // whole trimmed line, and the table ends at the next line that starts
    // with `[` after its indent. A commented header is not a table.
    let mut lines = text_lines(text);
    let (start, line) = lines.find(|(_, l)| js_trim(l) == header)?;
    let start = start + (line.len() - js_trim_start(line).len());
    let end = lines
        .find(|(_, l)| js_trim_start(l).starts_with('['))
        .map_or(text.len(), |(at, _)| at);
    Some((start, end))
}

/// Yields each line of `text` with its byte offset. The line keeps no
/// `\n`.
fn text_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    text.split('\n').map(move |line| {
        let start = at;
        at += line.len() + 1;
        (start, line)
    })
}

/// Builds `.cursor/hooks.json`'s content: three Cursor hook events, each
/// running `sieve hook <sub>` directly (Sieve resolves its own binary by
/// `PATH`, so no local shim file is needed — see the ledger entry of
/// 2026-09-12, "Sieve writes no .cjs shim").
pub fn cursor_hooks_json() -> String {
    let doc = OJson::obj(vec![
        ("version", OJson::Num(1)),
        (
            "hooks",
            OJson::obj(vec![
                (
                    "postToolUse",
                    OJson::arr(vec![OJson::obj(vec![
                        ("matcher", OJson::s("Read|Grep|Glob|Search|Shell")),
                        ("command", OJson::s(names::hook_command("cursor-post-tool"))),
                    ])]),
                ),
                (
                    "afterMCPExecution",
                    OJson::arr(vec![OJson::obj(vec![(
                        "command",
                        OJson::s(names::hook_command("cursor-mcp")),
                    )])]),
                ),
                (
                    "sessionEnd",
                    OJson::arr(vec![OJson::obj(vec![(
                        "command",
                        OJson::s(names::hook_command("cursor-session-end")),
                    )])]),
                ),
            ]),
        ),
    ]);
    doc.to_pretty() + "\n"
}

/// Writes `.cursor/hooks.json`, overwriting on any content change (there
/// is no foreign content to preserve here: Sieve owns every key in this
/// file).
pub fn write_cursor_hooks(path: &Path) -> io::Result<WriteAction> {
    Ok(match write_owned(path, &cursor_hooks_json())? {
        WriteAction::Replaced => WriteAction::Updated,
        other => other,
    })
}

/// Returns `true` when a hook entry's command already invokes Sieve's own
/// hook dispatcher (`sieve hook <sub>`) (section 1.7):
/// only such an entry is ever replaced on a re-run.
fn hook_entry_is_ours(entry: &OJson) -> bool {
    entry
        .get("hooks")
        .and_then(OJson::as_arr)
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(OJson::as_str)
                    .map(hook_command_is_ours)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Returns `true` when a statusline command is already Sieve's own.
fn statusline_is_ours(command: &str) -> bool {
    command == names::statusline_command() || command.contains(&product().statusline_file())
}

/// `true` when a hook event holds a value that is neither an array nor
/// null. A merge must not replace such a value.
fn bad_event_shape(event: Option<&OJson>) -> bool {
    event.is_some_and(|v| !matches!(v, OJson::Arr(_) | OJson::Null))
}

/// `true` when `hooks` is not an object (or null), or one event is not an
/// array (or null).
fn bad_hooks_shape(hooks: Option<&OJson>) -> bool {
    match hooks {
        None | Some(OJson::Null) => false,
        Some(OJson::Obj(pairs)) => pairs.iter().any(|(_, v)| bad_event_shape(Some(v))),
        Some(_) => true,
    }
}

/// `true` when `permissions` is not an object (or null), or `allow` is not
/// an array (or null).
fn bad_permissions_shape(permissions: Option<&OJson>) -> bool {
    match permissions {
        None | Some(OJson::Null) => false,
        Some(p @ OJson::Obj(_)) => bad_event_shape(p.get("allow")),
        Some(_) => true,
    }
}

/// The hook sub-commands `init` writes into a Claude or Cursor config.
const HOOK_SUBS: [&str; 10] = [
    "post-edit",
    "tool-savings",
    "post-read",
    "pre-read",
    "prompt",
    "session-start",
    "stop",
    "cursor-post-tool",
    "cursor-mcp",
    "cursor-session-end",
];

/// Returns `true` when `command` is, as a whole string, one hook command
/// `init` writes, or an older shim command. A user command such as
/// `my-sieve hook x` is not ours.
fn hook_command_is_ours(command: &str) -> bool {
    HOOK_SUBS.iter().any(|s| command == names::hook_command(s))
        || command.contains(&product().hooks_file())
}

/// Returns `true` when a `permissions.allow` entry is one of Sieve's own
/// invocation forms:
/// `^Bash\((?:sieve|npx sieve|sieve-dev|node dist\/cli\.js)(?::|\))`.
/// The prefix match drops an older form such as `Bash(sieve)`. A user
/// entry such as `Bash(sieve-mytool:*)` stays.
fn allow_entry_is_ours(entry: &str) -> bool {
    let name = product().name;
    let heads = [
        format!("Bash({name}"),
        format!("Bash(npx {name}"),
        format!("Bash({name}-dev"),
        "Bash(node dist/cli.js".to_string(),
    ];
    heads.iter().any(|head| {
        entry
            .strip_prefix(head.as_str())
            .is_some_and(|rest| rest.starts_with(':') || rest.starts_with(')'))
    })
}

/// The four `permissions.allow` entries `sieve init` always appends,
/// branded for the active product.
fn allow_entries() -> [String; 4] {
    let name = product().name;
    [
        format!("Bash({name}:*)"),
        format!("Bash(npx {name}:*)"),
        format!("Bash({name}-dev:*)"),
        "Bash(node dist/cli.js:*)".to_string(),
    ]
}

/// Returns `true` when a `footerLinksRegexes` entry contains the context
/// dir name, matching the note's drop rule (section 1.7): both Sieve's own
/// prior entry and any other regex that happens to name the context dir.
fn footer_entry_is_ours(entry: &str) -> bool {
    entry.contains(&format!("{}/", product().context_dir_name()))
}

/// Merges one Claude hook event's array: keeps every existing entry that
/// is not Sieve's own, in its existing order, then appends Sieve's fixed
/// blocks for that event (section 1.7, plus the keep-foreign
/// rule).
fn merged_hook_event(existing_hooks: Option<&OJson>, event: &str, ours: Vec<OJson>) -> OJson {
    let mut kept: Vec<OJson> = existing_hooks
        .and_then(|h| h.get(event))
        .and_then(OJson::as_arr)
        .map(|entries| {
            entries
                .iter()
                .filter(|e| !hook_entry_is_ours(e))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    kept.extend(ours);
    OJson::Arr(kept)
}

fn claude_hook_block(matcher: Option<&str>, sub: &str, timeout: i64) -> OJson {
    let handler = OJson::obj(vec![
        ("type", OJson::s("command")),
        ("command", OJson::s(names::hook_command(sub))),
        ("timeout", OJson::Num(timeout)),
    ]);
    match matcher {
        Some(m) => OJson::obj(vec![
            ("matcher", OJson::s(m)),
            ("hooks", OJson::arr(vec![handler])),
        ]),
        None => OJson::obj(vec![("hooks", OJson::arr(vec![handler]))]),
    }
}

/// Builds the merged `hooks` object: every managed event keeps its
/// foreign entries first, then gets Sieve's own blocks appended; any
/// other, unmanaged event (a foreign hook this merge never touches)
/// survives untouched.
///
/// F3, "remembered reads", and F1, "narrow a large Read", add a `PreToolUse`
/// block and the `post-read` blocks.
fn merged_hooks_ojson(existing_hooks: Option<&OJson>) -> OJson {
    let mut hooks = OMap::from_existing(existing_hooks);
    // A managed event that exists keeps its place; a new one is appended.
    let mut post_tool_use = vec![
        claude_hook_block(Some("Write|Edit|MultiEdit"), "post-edit", 10000),
        claude_hook_block(
            Some(&format!("Bash|mcp__{}__|Read|Grep|Glob", product().name)),
            "tool-savings",
            8000,
        ),
    ];
    post_tool_use.push(claude_hook_block(Some("Read"), "post-read", 5000));
    post_tool_use.push(claude_hook_block(Some("Bash"), "post-read", 5000));
    hooks.set(
        "PostToolUse",
        merged_hook_event(existing_hooks, "PostToolUse", post_tool_use),
    );
    hooks.set(
        "UserPromptSubmit",
        merged_hook_event(
            existing_hooks,
            "UserPromptSubmit",
            vec![claude_hook_block(None, "prompt", 15000)],
        ),
    );
    hooks.set(
        "SessionStart",
        merged_hook_event(
            existing_hooks,
            "SessionStart",
            vec![claude_hook_block(None, "session-start", 8000)],
        ),
    );
    hooks.set(
        "Stop",
        merged_hook_event(
            existing_hooks,
            "Stop",
            vec![claude_hook_block(None, "stop", 8000)],
        ),
    );
    hooks.set(
        "PreToolUse",
        merged_hook_event(
            existing_hooks,
            "PreToolUse",
            vec![
                claude_hook_block(Some("Read"), "pre-read", 5000),
                claude_hook_block(Some("Bash"), "pre-read", 5000),
            ],
        ),
    );
    hooks.into_ojson()
}

/// The `.claude/settings.json` merge (section 1.7).
/// Keeps every foreign top-level key, every foreign hook entry (per
/// event), `permissions.deny`, and any other `permissions` sub-key,
/// untouched and in their existing order. Replaces the statusline only
/// when it is absent or already Sieve's own; a foreign statusline is
/// kept as-is and reported through the returned warning list. Drops only
/// Sieve's own prior footer regex and allowlist entries before
/// re-appending its own.
///
/// Returns the write action and any warnings to print (section 1.7,
/// `applyStatusline`'s foreign-value case).
pub fn merge_claude_settings(
    path: &Path,
    no_statusline: bool,
) -> io::Result<(WriteAction, Vec<String>)> {
    let existed = path.is_file();
    let existing_text = if existed {
        Some(fs::read_to_string(path)?)
    } else {
        None
    };
    let existing_value = existing_text
        .as_deref()
        .and_then(OJson::parse)
        .filter(|v| matches!(v, OJson::Obj(_)));
    if existing_text.is_some() && existing_value.is_none() {
        return Ok((WriteAction::SkippedUnparseable, Vec::new()));
    }
    let existing_oj = existing_value;
    // A managed container of the wrong type is the user's value: write
    // nothing (the JSON key merge and the codex merge skip the same way).
    if existing_oj.as_ref().is_some_and(|o| {
        bad_hooks_shape(o.get("hooks")) || bad_permissions_shape(o.get("permissions"))
    }) {
        return Ok((WriteAction::SkippedUnparseable, Vec::new()));
    }
    let mut root = OMap::from_existing(existing_oj.as_ref());
    let mut warnings = Vec::new();

    // A managed key that exists keeps its place, and a new one is
    // appended in this call order (the merge
    // assigns into a copy of the file's own object).
    let statusline = OJson::obj(vec![
        ("type", OJson::s("command")),
        ("command", OJson::s(names::statusline_command())),
    ]);
    let existing_statusline = existing_oj.as_ref().and_then(|o| o.get("statusLine"));
    apply_statusline(
        &mut root,
        "statusLine",
        existing_statusline,
        &statusline,
        no_statusline,
        &mut warnings,
    );
    let existing_subagent = existing_oj
        .as_ref()
        .and_then(|o| o.get("subagentStatusLine"));
    apply_statusline(
        &mut root,
        "subagentStatusLine",
        existing_subagent,
        &statusline,
        no_statusline,
        &mut warnings,
    );

    root.set(
        "hooks",
        merged_hooks_ojson(existing_oj.as_ref().and_then(|o| o.get("hooks"))),
    );

    let existing_footer = existing_oj
        .as_ref()
        .and_then(|o| o.get("footerLinksRegexes"))
        .and_then(OJson::as_arr)
        .map(|items| {
            items
                .iter()
                .filter(|e| !e.as_str().map(footer_entry_is_ours).unwrap_or(false))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut footer = existing_footer;
    footer.push(OJson::s(format!(
        "{}/[\\w./-]+\\.md",
        product().context_dir_name()
    )));
    root.set("footerLinksRegexes", OJson::Arr(footer));

    let existing_permissions = existing_oj.as_ref().and_then(|o| o.get("permissions"));
    let mut permissions = OMap::from_existing(existing_permissions);
    let existing_allow = existing_permissions
        .and_then(|p| p.get("allow"))
        .and_then(OJson::as_arr)
        .map(|items| {
            items
                .iter()
                .filter(|e| !e.as_str().map(allow_entry_is_ours).unwrap_or(false))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut allow = existing_allow;
    allow.extend(allow_entries().into_iter().map(OJson::s));
    permissions.set("allow", OJson::Arr(allow));
    root.set("permissions", permissions.into_ojson());

    let new_text = root.into_ojson().to_pretty() + "\n";
    if existing_text.as_deref() == Some(new_text.as_str()) {
        return Ok((WriteAction::Unchanged, warnings));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, new_text)?;
    Ok((
        if existed {
            WriteAction::Replaced
        } else {
            WriteAction::Created
        },
        warnings,
    ))
}

/// Sets or keeps one statusline key, matching `applyStatusline`: replaces
/// an absent or already-ours value; keeps a foreign value untouched and
/// queues a warning; `--no-statusline` deletes an owned value instead of
/// setting it, and still warns when a foreign value blocked the delete.
fn apply_statusline(
    root: &mut OMap,
    key: &str,
    existing: Option<&OJson>,
    wanted: &OJson,
    no_statusline: bool,
    warnings: &mut Vec<String>,
) {
    // Any truthy value that is not its own statusline is foreign
    // (even a bare string).
    let truthy = existing.is_some_and(|v| match v {
        OJson::Null | OJson::Bool(false) | OJson::Num(0) => false,
        OJson::Str(s) => !s.is_empty(),
        _ => true,
    });
    let current_is_foreign = truthy
        && !existing
            .and_then(|v| v.get("command"))
            .and_then(OJson::as_str)
            .is_some_and(statusline_is_ours);
    if no_statusline {
        if current_is_foreign {
            warnings.push(statusline_warning(key));
            if let Some(v) = existing {
                root.set(key, v.clone());
            }
        } else {
            root.remove(key);
        }
        return;
    }
    if current_is_foreign {
        warnings.push(statusline_warning(key));
        if let Some(v) = existing {
            root.set(key, v.clone());
        }
    } else {
        root.set(key, wanted.clone());
    }
}

fn statusline_warning(key: &str) -> String {
    if key == "statusLine" {
        format!(
            "Existing statusLine left untouched (a session allows only one). To use Sieve, point it at `{}`.",
            names::statusline_command()
        )
    } else {
        "Existing subagentStatusLine left untouched.".to_string()
    }
}

/// The `~/.claude/settings.json` mirror `sieve init` merges into when
/// `$HOME/.claude` already exists (section 1.9). Touches only the
/// `hooks` key, with the same five blocks the local `.claude/settings.json`
/// merge writes; every other top-level key, and every foreign hook entry,
/// survives untouched. No write when the merged text equals the file on
/// disk.
pub fn merge_claude_global_hooks(path: &Path) -> io::Result<WriteAction> {
    let existed = path.is_file();
    let existing_text = if existed {
        Some(fs::read_to_string(path)?)
    } else {
        None
    };
    let existing_value = existing_text
        .as_deref()
        .and_then(OJson::parse)
        .filter(|v| matches!(v, OJson::Obj(_)));
    if existing_text.is_some() && existing_value.is_none() {
        return Ok(WriteAction::SkippedUnparseable);
    }
    let existing_oj = existing_value;
    if existing_oj
        .as_ref()
        .is_some_and(|o| bad_hooks_shape(o.get("hooks")))
    {
        return Ok(WriteAction::SkippedUnparseable);
    }
    let mut root = OMap::from_existing(existing_oj.as_ref());
    root.set(
        "hooks",
        merged_hooks_ojson(existing_oj.as_ref().and_then(|o| o.get("hooks"))),
    );
    let new_text = root.into_ojson().to_pretty() + "\n";
    if existing_text.as_deref() == Some(new_text.as_str()) {
        return Ok(WriteAction::Unchanged);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, new_text)?;
    Ok(if existed {
        WriteAction::Replaced
    } else {
        WriteAction::Created
    })
}

/// Removes Sieve's `hooks` entries from `~/.claude/settings.json` for
/// `sieve uninstall` (section 1.9). Touches only the `hooks` key, the
/// same key [`merge_claude_global_hooks`] writes; every other top-level
/// key survives untouched. Deletes the whole file when nothing else is
/// left. Returns a [`Retract`].
pub fn strip_claude_global_hooks(path: &Path, apply: bool) -> io::Result<Retract> {
    let oj = match json_root(path)? {
        Ok(oj) => oj,
        Err(state) => return Ok(state),
    };
    let mut root = OMap::from_existing(Some(&oj));

    let Some(hooks) = oj.get("hooks").filter(|h| h.as_obj().is_some()) else {
        return Ok(Retract::Absent);
    };
    let mut hooks_map = OMap::from_existing(Some(hooks));
    // Every event key. The sieve product also registers `PreToolUse`
    // in the global file.
    let touched = strip_ours_in(&mut hooks_map, hooks, &object_keys(hooks));
    if !touched {
        return Ok(Retract::Absent);
    }
    if hooks_map.is_empty() {
        root.remove("hooks");
    } else {
        root.set("hooks", hooks_map.into_ojson());
    }

    let emptied = root.is_empty();
    finish(
        path,
        (!emptied).then(|| root.into_ojson().to_pretty() + "\n"),
        apply,
    )
}

/// The four Codex events an older Sieve wrote at the top level of
/// `~/.codex/hooks.json`, before it matched the nested `hooks` key.
const CODEX_EVENTS: [&str; 4] = ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"];

/// The keys of an object value, in order.
fn object_keys(obj: &OJson) -> Vec<String> {
    obj.as_obj()
        .map(|pairs| pairs.iter().map(|(k, _)| k.clone()).collect())
        .unwrap_or_default()
}

/// Drops Sieve's own entries from each named event array of `src`, in
/// `map` (a copy of `src`). An event left empty is removed. Returns `true`
/// when any entry went.
fn strip_ours_in(map: &mut OMap, src: &OJson, events: &[String]) -> bool {
    let mut touched = false;
    for event in events {
        let Some(existing) = src.get(event).and_then(OJson::as_arr) else {
            continue;
        };
        let kept: Vec<OJson> = existing
            .iter()
            .filter(|e| !hook_entry_is_ours(e))
            .cloned()
            .collect();
        if kept.len() != existing.len() {
            touched = true;
        }
        if kept.is_empty() {
            map.remove(event);
        } else {
            map.set(event, OJson::Arr(kept));
        }
    }
    touched
}

/// Removes Sieve's hook events from `~/.codex/hooks.json` for `sieve
/// uninstall` (section 1.10). Sieve nests the events under a top-level
/// `hooks` key, so this strips every event there.
/// It also strips the four events at the top level, where an older Sieve
/// wrote them. Keeps every foreign entry. Deletes the whole file when
/// nothing else is left. Returns a [`Retract`].
pub fn strip_codex_hooks(path: &Path, apply: bool) -> io::Result<Retract> {
    let oj = match json_root(path)? {
        Ok(oj) => oj,
        Err(state) => return Ok(state),
    };
    // A `hooks` value that is not an object is the user's: leave
    // the file as it is.
    if !matches!(
        oj.get("hooks"),
        None | Some(OJson::Null) | Some(OJson::Obj(_))
    ) {
        return Ok(Retract::Absent);
    }
    let mut root = OMap::from_existing(Some(&oj));
    let legacy = CODEX_EVENTS.map(String::from);
    let mut touched = strip_ours_in(&mut root, &oj, &legacy);
    if let Some(hooks) = oj.get("hooks").filter(|h| h.as_obj().is_some()) {
        let mut hooks_map = OMap::from_existing(Some(hooks));
        if strip_ours_in(&mut hooks_map, hooks, &object_keys(hooks)) {
            touched = true;
            if hooks_map.is_empty() {
                root.remove("hooks");
            } else {
                root.set("hooks", hooks_map.into_ojson());
            }
        }
    }
    if !touched {
        return Ok(Retract::Absent);
    }
    let emptied = root.is_empty();
    finish(
        path,
        (!emptied).then(|| root.into_ojson().to_pretty() + "\n"),
        apply,
    )
}

/// Builds the merged `hooks` object of the Codex `hooks.json` (section
/// 1.10): the four events. Keeps every foreign
/// entry per event, then appends Sieve's own block.
fn merged_codex_hooks_ojson(existing: Option<&OJson>) -> OJson {
    let mut hooks = OMap::from_existing(existing);
    hooks.set(
        "SessionStart",
        merged_hook_event(
            existing,
            "SessionStart",
            vec![claude_hook_block(
                Some("startup|resume|compact"),
                "session-start",
                10000,
            )],
        ),
    );
    hooks.set(
        "UserPromptSubmit",
        merged_hook_event(
            existing,
            "UserPromptSubmit",
            vec![claude_hook_block(None, "prompt", 15000)],
        ),
    );
    hooks.set(
        "PostToolUse",
        merged_hook_event(
            existing,
            "PostToolUse",
            vec![claude_hook_block(
                Some("apply_patch|Write|Edit|MultiEdit"),
                "post-edit",
                10000,
            )],
        ),
    );
    hooks.set(
        "Stop",
        merged_hook_event(
            existing,
            "Stop",
            vec![claude_hook_block(None, "stop", 10000)],
        ),
    );
    hooks.into_ojson()
}

/// Merges Sieve's four hook events into `~/.codex/hooks.json` (section
/// 1.10). Keeps every foreign entry, then appends Sieve's own block per
/// event. No write when the merged text equals the file on disk.
pub fn merge_codex_hooks(path: &Path) -> io::Result<WriteAction> {
    let existed = path.is_file();
    let existing_text = if existed {
        Some(fs::read_to_string(path)?)
    } else {
        None
    };
    let existing_value = existing_text
        .as_deref()
        .and_then(OJson::parse)
        .filter(|v| matches!(v, OJson::Obj(_)));
    if existing_text.is_some() && existing_value.is_none() {
        return Ok(WriteAction::SkippedUnparseable);
    }
    let existing_oj = existing_value;
    // Skip a non-object `hooks` or a non-array event.
    let hooks = existing_oj.as_ref().and_then(|o| o.get("hooks"));
    if !matches!(hooks, None | Some(OJson::Null) | Some(OJson::Obj(_)))
        || CODEX_EVENTS
            .iter()
            .any(|e| bad_event_shape(hooks.and_then(|h| h.get(e))))
    {
        return Ok(WriteAction::SkippedUnparseable);
    }
    let mut root = OMap::from_existing(existing_oj.as_ref());
    // An older Sieve wrote the events at the top level. Drop its entries
    // there, so the nested block does not double them.
    if let Some(o) = existing_oj.as_ref() {
        strip_ours_in(&mut root, o, &CODEX_EVENTS.map(String::from));
    }
    root.set("hooks", merged_codex_hooks_ojson(hooks));
    let new_text = root.into_ojson().to_pretty() + "\n";
    if existing_text.as_deref() == Some(new_text.as_str()) {
        return Ok(WriteAction::Unchanged);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, new_text)?;
    Ok(if existed {
        WriteAction::Updated
    } else {
        WriteAction::Created
    })
}

/// Removes Sieve's own keys from `.claude/settings.json` for `sieve
/// uninstall`. Keeps every foreign hook entry, keeps a foreign
/// statusline, keeps `permissions.deny` and any other key. Deletes the
/// whole file when nothing else is left. Returns a [`Retract`]; with `apply`
/// false it only reports.
pub fn strip_claude_settings(path: &Path, apply: bool) -> io::Result<Retract> {
    let oj = match json_root(path)? {
        Ok(oj) => oj,
        Err(state) => return Ok(state),
    };
    let mut root = OMap::from_existing(Some(&oj));

    for key in ["statusLine", "subagentStatusLine"] {
        let is_ours = oj
            .get(key)
            .and_then(|v| v.get("command"))
            .and_then(OJson::as_str)
            .map(statusline_is_ours)
            .unwrap_or(false);
        if is_ours {
            root.remove(key);
        }
    }

    // Sieve walks every event key, and touches
    // `hooks` only when it is an object.
    if let Some(hooks) = oj.get("hooks").filter(|h| h.as_obj().is_some()) {
        let mut hooks_map = OMap::from_existing(Some(hooks));
        let events: Vec<String> = hooks
            .as_obj()
            .map(|pairs| pairs.iter().map(|(k, _)| k.clone()).collect())
            .unwrap_or_default();
        for event in events.iter().map(String::as_str) {
            let Some(existing) = hooks.get(event).and_then(OJson::as_arr) else {
                continue;
            };
            let kept: Vec<OJson> = existing
                .iter()
                .filter(|e| !hook_entry_is_ours(e))
                .cloned()
                .collect();
            if kept.is_empty() {
                hooks_map.remove(event);
            } else {
                hooks_map.set(event, OJson::Arr(kept));
            }
        }
        if hooks_map.is_empty() {
            root.remove("hooks");
        } else {
            root.set("hooks", hooks_map.into_ojson());
        }
    }

    if let Some(footer) = oj.get("footerLinksRegexes").and_then(OJson::as_arr) {
        let kept: Vec<OJson> = footer
            .iter()
            .filter(|e| !e.as_str().map(footer_entry_is_ours).unwrap_or(false))
            .cloned()
            .collect();
        if kept.is_empty() {
            root.remove("footerLinksRegexes");
        } else {
            root.set("footerLinksRegexes", OJson::Arr(kept));
        }
    }

    // Touch `permissions` only when `permissions.allow` is an
    // array: `permissions: {}` stays.
    if let Some(permissions) = oj.get("permissions") {
        let mut permissions_map = OMap::from_existing(Some(permissions));
        if let Some(allow) = permissions.get("allow").and_then(OJson::as_arr) {
            let kept: Vec<OJson> = allow
                .iter()
                .filter(|e| !e.as_str().map(allow_entry_is_ours).unwrap_or(false))
                .cloned()
                .collect();
            if kept.is_empty() {
                permissions_map.remove("allow");
            } else {
                permissions_map.set("allow", OJson::Arr(kept));
            }
            if permissions_map.is_empty() {
                root.remove("permissions");
            } else {
                root.set("permissions", permissions_map.into_ojson());
            }
        }
    }

    // The key that `init --mod` set, only when its value is `true`. Other
    // plugin keys and any other value stay.
    if let Some(plugins) = oj
        .get("enabledPlugins")
        .filter(|p| matches!(p.get(crate::pane_mod::PLUGIN_KEY), Some(OJson::Bool(true))))
    {
        let mut plugins_map = OMap::from_existing(Some(plugins));
        plugins_map.remove(crate::pane_mod::PLUGIN_KEY);
        if plugins_map.is_empty() {
            root.remove("enabledPlugins");
        } else {
            root.set("enabledPlugins", plugins_map.into_ojson());
        }
    }

    let emptied = root.is_empty();
    let after = root.into_ojson();
    // No Sieve key in the file: leave it byte for byte (P4-49).
    if after.to_pretty() == oj.to_pretty() {
        return Ok(Retract::Absent);
    }
    finish(path, (!emptied).then(|| after.to_pretty() + "\n"), apply)
}

/// Strips the block `ensure_gitignored` or `ensure_searchable` wrote from
/// `.gitignore` or `.ignore`, and deletes the file when nothing else is left.
/// The block must match as whole lines, so a line the user wrote stays (a
/// looser strip also removes a lone entry or any comment that names the
/// product; Sieve does not). The blank line `ensure_entry` put before the block
/// goes with it.
fn strip_ignore_file(path: &Path, block: &str, apply: bool) -> io::Result<Retract> {
    if !path.is_file() {
        return Ok(Retract::Absent);
    }
    let Ok(current) = fs::read_to_string(path) else {
        return Ok(Retract::Absent);
    };
    // Lines with their own `\n`, so the user's final newline survives.
    let lines: Vec<&str> = current.split_inclusive('\n').collect();
    let bare = |i: usize| lines[i].trim_end_matches('\n').trim_end_matches('\r');
    let want: Vec<&str> = block.trim_end().split('\n').collect();
    let Some(at) = (0..=lines.len().saturating_sub(want.len())).find(|&i| {
        i + want.len() <= lines.len() && want.iter().enumerate().all(|(k, w)| bare(i + k) == *w)
    }) else {
        return Ok(Retract::Absent);
    };
    // `ensure_entry` puts one blank line before the block of a file that
    // already had text. That line goes; any other blank line stays.
    let start = if at > 0 && bare(at - 1).is_empty() {
        at - 1
    } else {
        at
    };
    let remainder = [&lines[..start], &lines[at + want.len()..]]
        .concat()
        .concat();
    finish(
        path,
        (!js_trim(&remainder).is_empty()).then_some(remainder),
        apply,
    )
}

/// Strips the product's `.gitignore` block for its context dir.
pub fn strip_gitignore(path: &Path, apply: bool) -> io::Result<Retract> {
    let bare = product().context_dir_name();
    strip_ignore_file(path, &sieve_core::ignore::gitignore_block(bare), apply)
}

/// Strips the product's `.ignore` block for its context dir.
pub fn strip_ignore(path: &Path, apply: bool) -> io::Result<Retract> {
    let bare = product().context_dir_name();
    strip_ignore_file(path, &sieve_core::ignore::ignore_block(bare), apply)
}

/// Reads `<context_dir>/.graph/wiring.json` and returns its node and edge
/// counts, or `None` when no graph is present yet.
pub fn graph_counts(context_dir: &Path) -> Option<(usize, usize)> {
    let path = context_dir.join(".graph").join("wiring.json");
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let nodes = value.get("nodes")?.as_array()?.len();
    let edges = value.get("edges")?.as_array()?.len();
    Some((nodes, edges))
}

/// Renders the closing epilogue `sieve init` prints to stderr after every
/// target write (section 1.11, `formatInitEpilogue`), with no ANSI color
/// (Sieve never colors this report — the goldens capture a non-TTY run).
///
/// `counts` is `None` on a repo that still has no graph — a build failure
/// (P4-01: a failed in-process build never stops the rest of `init`) or a
/// `--no-build` run on a fresh repo. The epilogue then prepends a fourth
/// step, "1. build the graph",
/// renumbering the rest, and drops the node/edge stats from the wordmark
/// (a repo where `sieve` exists as a plain file forces the build to fail).
pub fn epilogue(counts: Option<(usize, usize)>) -> String {
    let counts_suffix = match counts {
        Some((nodes, edges)) => format!("  {nodes} nodes · {edges} edges"),
        None => String::new(),
    };
    let steps = if counts.is_some() {
        "\n  1. restart your agent  a new session picks up sieve automatically\n  2. code as usual       ask your agent to fix a bug or explain a flow —\n                         it now answers from the graph\n  3. explore by hand     sieve ask \"where is auth handled?\" · sieve callers <fn> · sieve viz\n"
    } else {
        "\n  1. build the graph     sieve build\n  2. restart your agent  a new session picks up sieve automatically\n  3. code as usual       ask your agent to fix a bug or explain a flow —\n                         it now answers from the graph\n  4. explore by hand     sieve ask \"where is auth handled?\" · sieve callers <fn> · sieve viz\n"
    };
    format!(
        "\n       _\n   ___(_) _____   _____\n  / __| |/ _ \\ \\ / / _ \\\n  \\__ \\ |  __/\\ V /  __/\n  |___/_|\\___| \\_/ \\___|{counts_suffix}\n{steps}\n  share it: git add .claude && git commit — teammates run `sieve build` for their own local graph\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// The agent texts hold no tally instruction and name the true `why` form.
    #[test]
    fn test_templates_hold_no_tally_text_and_the_why_form() {
        for text in [skill_template(), instruction_body()] {
            for bad in ["\u{1f331}", "close your reply", "tally", "tokens saved"] {
                assert!(!text.contains(bad), "agent text holds {bad:?}");
            }
            assert!(text.contains("sieve why <symbol, file, or path:line>"));
            assert!(text.contains("for your current diff"));
        }
    }

    /// A temp dir that removes itself on drop (mirrors the pattern in
    /// `tests/ask_cli.rs`; duplicated here since a unit test cannot reach
    /// a sibling test binary's helper).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("sieve-hosts-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// P4-09: the allow match is a prefix match on the product's own
    /// invocation forms, followed by `:` or `)`.
    #[test]
    fn test_p4_09_allow_entry_is_ours_matches_the_golden_prefix_regex() {
        for own in [
            "Bash(sieve)",
            "Bash(sieve:*)",
            "Bash(npx sieve)",
            "Bash(npx sieve:*)",
            "Bash(sieve-dev:*)",
            "Bash(node dist/cli.js:*)",
            "Bash(sieve:build)",
        ] {
            assert!(allow_entry_is_ours(own), "must drop {own}");
        }
        for foreign in [
            "Bash(sieve-mytool:*)",
            "Bash(ls:*)",
            "Bash(other:*)",
            "Bash(mysieve:*)",
            "Read(sieve)",
            "Bash(sieve",
        ] {
            assert!(!allow_entry_is_ours(foreign), "must keep {foreign}");
        }
    }

    #[test]
    fn write_owned_reports_created_then_unchanged_then_replaced() {
        let dir = TempDir::new("write-owned");
        let path = dir.path.join("a/b/file.md");
        assert_eq!(write_owned(&path, "one").unwrap(), WriteAction::Created);
        assert_eq!(write_owned(&path, "one").unwrap(), WriteAction::Unchanged);
        assert_eq!(write_owned(&path, "two").unwrap(), WriteAction::Replaced);
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn upsert_section_creates_then_is_idempotent() {
        let dir = TempDir::new("upsert-create");
        let path = dir.path.join("AGENTS.md");
        assert_eq!(
            upsert_section(&path, "body text").unwrap(),
            WriteAction::Created
        );
        let first = fs::read_to_string(&path).unwrap();
        assert!(first.starts_with(&section_start()));
        assert_eq!(
            upsert_section(&path, "body text").unwrap(),
            WriteAction::Unchanged
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn upsert_section_replaces_an_existing_block_between_markers() {
        let dir = TempDir::new("upsert-replace");
        let path = dir.path.join("AGENTS.md");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            "before\n{}\nold\n{}\nafter",
            section_start(),
            section_end()
        )
        .unwrap();
        drop(f);
        upsert_section(&path, "new body").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("new body"));
        assert!(content.starts_with("before\n"));
        assert!(content.trim_end().ends_with("after"));
    }

    #[test]
    fn strip_section_deletes_a_file_that_holds_only_the_block() {
        let dir = TempDir::new("strip-section");
        let path = dir.path.join("AGENTS.md");
        upsert_section(&path, "body").unwrap();
        assert!(strip_section(&path, true).unwrap().hit());
        assert!(!path.exists());
    }

    #[test]
    fn merge_mcp_json_creates_then_is_idempotent_then_strips_to_deletion() {
        let dir = TempDir::new("mcp-json");
        let path = dir.path.join(".mcp.json");
        assert_eq!(
            merge_mcp_json(&path, "mcpServers").unwrap(),
            WriteAction::Created
        );
        assert_eq!(
            merge_mcp_json(&path, "mcpServers").unwrap(),
            WriteAction::Unchanged
        );
        assert!(strip_mcp_json(&path, "mcpServers", true).unwrap().hit());
        assert!(!path.exists());
    }

    #[test]
    fn merge_mcp_json_skips_an_unparseable_existing_file() {
        let dir = TempDir::new("mcp-json-bad");
        let path = dir.path.join(".mcp.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(
            merge_mcp_json(&path, "mcpServers").unwrap(),
            WriteAction::SkippedUnparseable
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");
    }

    #[test]
    fn merge_claude_settings_is_idempotent_and_strips_to_deletion() {
        let dir = TempDir::new("claude-settings");
        let path = dir.path.join("settings.json");
        let (action, _) = merge_claude_settings(&path, false).unwrap();
        assert_eq!(action, WriteAction::Created);
        let (action, _) = merge_claude_settings(&path, false).unwrap();
        assert_eq!(action, WriteAction::Unchanged);
        assert!(strip_claude_settings(&path, true).unwrap().hit());
        assert!(!path.exists());
    }

    #[test]
    fn merge_claude_settings_keeps_a_foreign_hook_entry_before_ours() {
        let dir = TempDir::new("claude-settings-foreign-hook");
        let path = dir.path.join("settings.json");
        fs::write(
            &path,
            r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "my-own-tool", "timeout": 5000 }
        ]
      }
    ]
  }
}
"#,
        )
        .unwrap();
        let (action, warnings) = merge_claude_settings(&path, false).unwrap();
        assert_eq!(action, WriteAction::Replaced);
        assert!(warnings.is_empty());
        let written = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        let post_tool_use = value["hooks"]["PostToolUse"].as_array().unwrap();
        // The foreign entry survives, in its original position, ahead of
        // Sieve's own four blocks.
        assert_eq!(post_tool_use.len(), 5);
        assert_eq!(post_tool_use[0]["matcher"], "Write");
        assert_eq!(post_tool_use[0]["hooks"][0]["command"], "my-own-tool");
        assert!(post_tool_use[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("hook post-edit"));

        // A second run is a no-op: the foreign entry is still not ours,
        // so it survives again with no duplication.
        let (action, _) = merge_claude_settings(&path, false).unwrap();
        assert_eq!(action, WriteAction::Unchanged);
    }

    #[test]
    fn merge_claude_settings_keeps_a_foreign_statusline_and_warns() {
        let dir = TempDir::new("claude-settings-foreign-statusline");
        let path = dir.path.join("settings.json");
        fs::write(
            &path,
            r#"{"statusLine": {"type": "command", "command": "my-own-statusline.sh"}}"#,
        )
        .unwrap();
        let (action, warnings) = merge_claude_settings(&path, false).unwrap();
        assert_eq!(action, WriteAction::Replaced);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("Existing statusLine left untouched"));
        let written = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(value["statusLine"]["command"], "my-own-statusline.sh");

        // A second run keeps warning the same way and stays a no-op on
        // disk (the foreign value is never touched).
        let (action, warnings) = merge_claude_settings(&path, false).unwrap();
        assert_eq!(action, WriteAction::Unchanged);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn merge_claude_global_hooks_keeps_a_foreign_hook_before_ours_and_is_idempotent() {
        let dir = TempDir::new("claude-global-hooks");
        let path = dir.path.join(".claude").join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "my-own-tool"}]}]}}"#,
        )
        .unwrap();

        let action = merge_claude_global_hooks(&path).unwrap();
        assert_eq!(action, WriteAction::Replaced);
        let written = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        let stop = value["hooks"]["Stop"].as_array().unwrap();
        // The foreign hook survives, ahead of Sieve's own block.
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "my-own-tool");
        assert!(stop[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("hook stop"));

        // A second run is a no-op.
        let action = merge_claude_global_hooks(&path).unwrap();
        assert_eq!(action, WriteAction::Unchanged);
    }

    /// The expected `~/.codex/hooks.json` for a fresh home, with the
    /// shim path masked as `<SHIM>`. Sieve's command replaces the shim form.
    fn golden_codex_hooks_bytes() -> String {
        include_str!("../../../tests/fixtures/codex-hooks.sieve.json")
            .replace("node \\\"<SHIM>\\\" ", &names::hook_command(""))
    }

    const OLD_TOP_LEVEL: &str = r#"{"Stop": [{"hooks": [{"type": "command", "command": "my-own-tool"}]}, {"hooks": [{"type": "command", "command": "sieve hook stop", "timeout": 10000}]}], "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "sieve hook prompt", "timeout": 15000}]}]}"#;

    #[test]
    fn test_p4_50_codex_hooks_shape_fresh_write_matches_golden_bytes() {
        let dir = TempDir::new("codex-hooks");
        let path = dir.path.join(".codex").join("hooks.json");

        assert_eq!(merge_codex_hooks(&path).unwrap(), WriteAction::Created);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            golden_codex_hooks_bytes()
        );
        // A second run is a no-op.
        assert_eq!(merge_codex_hooks(&path).unwrap(), WriteAction::Unchanged);
        // The strip leaves nothing, so the file goes.
        assert!(strip_codex_hooks(&path, true).unwrap().hit());
        assert!(!path.exists());
    }

    #[test]
    fn test_p4_50_codex_hooks_shape_merge_and_strip_keep_user_entries() {
        let dir = TempDir::new("codex-hooks");
        let path = dir.path.join(".codex").join("hooks.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user = r#"{
  "zeta": 1.0,
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "my-own-tool"
          }
        ]
      }
    ]
  }
}
"#;
        fs::write(&path, user).unwrap();

        assert_eq!(merge_codex_hooks(&path).unwrap(), WriteAction::Updated);
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value.get("Stop").is_none(), "no top-level event");
        let stop = value["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "my-own-tool");
        assert_eq!(stop[1]["hooks"][0]["command"], "sieve hook stop");
        assert_eq!(merge_codex_hooks(&path).unwrap(), WriteAction::Unchanged);

        // The strip restores the user's file, byte for byte but for the
        // float, which JS writes as `1`.
        assert!(strip_codex_hooks(&path, true).unwrap().hit());
        assert_eq!(fs::read_to_string(&path).unwrap(), user.replace("1.0", "1"));
        assert!(!strip_codex_hooks(&path, true).unwrap().hit());
    }

    #[test]
    fn test_p4_50_codex_hooks_shape_old_top_level_file_strips_and_migrates() {
        let dir = TempDir::new("codex-hooks");
        let path = dir.path.join(".codex").join("hooks.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Strip: only the user's own top-level entry stays.
        fs::write(&path, OLD_TOP_LEVEL).unwrap();
        assert!(strip_codex_hooks(&path, true).unwrap().hit());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\n  \"Stop\": [\n    {\n      \"hooks\": [\n        {\n          \"type\": \"command\",\n          \"command\": \"my-own-tool\"\n        }\n      ]\n    }\n  ]\n}\n"
        );

        // Merge: the user's top-level entry stays; Sieve's entries move
        // under `hooks` with no copy left at the top level.
        fs::write(&path, OLD_TOP_LEVEL).unwrap();
        assert_eq!(merge_codex_hooks(&path).unwrap(), WriteAction::Updated);
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["Stop"].as_array().unwrap().len(), 1);
        assert!(value.get("UserPromptSubmit").is_none());
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            1
        );
        assert_eq!(merge_codex_hooks(&path).unwrap(), WriteAction::Unchanged);
    }

    /// A user command that only resembles Sieve's (extra argument, full
    /// path) is the user's. Merge and strip keep it, top level or nested.
    #[test]
    fn test_p4_50_codex_hooks_shape_keeps_look_alike_user_commands() {
        let dir = TempDir::new("codex-hooks");
        let path = dir.path.join("hooks.json");
        let entry = |c: &str| format!(r#"{{"hooks":[{{"type":"command","command":"{c}"}}]}}"#);
        let seed = format!(
            r#"{{"Stop":[{a},{b}],"hooks":{{"Stop":[{a},{b}]}}}}"#,
            a = entry("sieve hook stop --extra"),
            b = entry("/usr/bin/sieve hook stop"),
        );
        fs::write(&path, &seed).unwrap();
        let before: serde_json::Value = serde_json::from_str(&seed).unwrap();

        merge_codex_hooks(&path).unwrap();
        let merged: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(merged["Stop"], before["Stop"]);
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 3);
        assert_eq!(
            merged["hooks"]["Stop"].as_array().unwrap()[..2],
            before["hooks"]["Stop"].as_array().unwrap()[..]
        );

        assert!(strip_codex_hooks(&path, true).unwrap().hit());
        let stripped: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stripped, before);
    }

    /// Sieve leaves a file whose `hooks` value is not an object, even if
    /// old top-level Sieve events sit beside it.
    #[test]
    fn test_p4_50_codex_hooks_shape_strip_leaves_a_bad_hooks_value() {
        let dir = TempDir::new("codex-hooks");
        let path = dir.path.join("hooks.json");
        let seed =
            r#"{"Stop":[{"hooks":[{"type":"command","command":"sieve hook stop"}]}],"hooks":[1]}"#;
        fs::write(&path, seed).unwrap();
        assert!(!strip_codex_hooks(&path, true).unwrap().hit());
        assert_eq!(fs::read_to_string(&path).unwrap(), seed);
    }

    #[test]
    fn test_p4_50_codex_hooks_shape_skips_a_bad_hooks_value() {
        let dir = TempDir::new("codex-hooks");
        let path = dir.path.join("hooks.json");
        for bad in [r#"{"hooks": []}"#, r#"{"hooks": {"Stop": {}}}"#] {
            fs::write(&path, bad).unwrap();
            assert_eq!(
                merge_codex_hooks(&path).unwrap(),
                WriteAction::SkippedUnparseable
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), bad);
        }
    }

    #[test]
    fn merge_mcp_toml_is_idempotent_and_strips_to_deletion() {
        let dir = TempDir::new("grok-toml");
        let path = dir.path.join("config.toml");
        assert_eq!(merge_mcp_toml(&path).unwrap(), WriteAction::Created);
        assert_eq!(merge_mcp_toml(&path).unwrap(), WriteAction::Unchanged);
        assert!(strip_mcp_toml(&path, true).unwrap().hit());
        assert!(!path.exists());
    }

    const USER_TOML: &str = "[mcp_servers.mine]\ncommand = \"foo\"\n\n[other]\nx = 1\n";

    /// The expected table after one blank line
    /// (the Codex TOML upsert). The command is Sieve's binary name.
    fn golden_toml_table() -> String {
        format!(
            "[mcp_servers.sieve]\ncommand = \"{}\"\nargs = [\"mcp\"]\n",
            names::MCP_COMMAND
        )
    }

    #[test]
    fn test_p4_51_codex_toml_appends_after_user_tables_and_strips_back() {
        let dir = TempDir::new("codex-toml");
        let path = dir.path.join("config.toml");
        fs::write(&path, USER_TOML).unwrap();

        assert_eq!(merge_mcp_toml(&path).unwrap(), WriteAction::Updated);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{USER_TOML}\n{}", golden_toml_table())
        );
        assert_eq!(merge_mcp_toml(&path).unwrap(), WriteAction::Unchanged);
        // The strip gives the user's file back byte for byte.
        assert_eq!(strip_mcp_toml(&path, false).unwrap(), Retract::Removed);
        assert_eq!(strip_mcp_toml(&path, true).unwrap(), Retract::Removed);
        assert_eq!(fs::read_to_string(&path).unwrap(), USER_TOML);
    }

    #[test]
    fn test_p4_51_codex_toml_leaves_a_file_that_is_not_text() {
        let dir = TempDir::new("codex-toml-binary");
        let path = dir.path.join("config.toml");
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();

        assert_eq!(
            merge_mcp_toml(&path).unwrap(),
            WriteAction::SkippedUnparseable
        );
        assert_eq!(fs::read(&path).unwrap(), [0xff, 0xfe, 0x00]);
    }

    /// The `opencode.json` bytes.
    fn golden_opencode_json(user_first: bool) -> String {
        let user = "    \"mine\": {\n      \"type\": \"local\",\n      \"command\": [\n        \"foo\"\n      ]\n    }";
        let ours = format!(
            "    \"sieve\": {{\n      \"type\": \"local\",\n      \"command\": [\n        \"{}\",\n        \"mcp\"\n      ],\n      \"enabled\": true\n    }}",
            names::MCP_COMMAND
        );
        if user_first {
            format!("{{\n  \"theme\": \"dark\",\n  \"mcp\": {{\n{user},\n{ours}\n  }}\n}}\n")
        } else {
            format!("{{\n  \"mcp\": {{\n{ours}\n  }}\n}}\n")
        }
    }

    #[test]
    fn test_p4_51_opencode_json_matches_golden_bytes_and_keeps_user_keys() {
        let dir = TempDir::new("opencode");
        let fresh = dir.path.join("fresh.json");
        assert_eq!(merge_opencode_json(&fresh).unwrap(), WriteAction::Created);
        assert_eq!(
            fs::read_to_string(&fresh).unwrap(),
            golden_opencode_json(false)
        );
        assert_eq!(merge_opencode_json(&fresh).unwrap(), WriteAction::Unchanged);
        assert_eq!(
            strip_mcp_json(&fresh, "mcp", true).unwrap(),
            Retract::Deleted
        );
        assert!(!fresh.exists());

        let user = dir.path.join("user.json");
        fs::write(
            &user,
            r#"{"theme":"dark","mcp":{"mine":{"type":"local","command":["foo"]}}}"#,
        )
        .unwrap();
        assert_eq!(merge_opencode_json(&user).unwrap(), WriteAction::Updated);
        assert_eq!(
            fs::read_to_string(&user).unwrap(),
            golden_opencode_json(true)
        );
        assert_eq!(
            strip_mcp_json(&user, "mcp", true).unwrap(),
            Retract::Removed
        );
        assert_eq!(
            fs::read_to_string(&user).unwrap(),
            "{\n  \"theme\": \"dark\",\n  \"mcp\": {\n    \"mine\": {\n      \"type\": \"local\",\n      \"command\": [\n        \"foo\"\n      ]\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn test_p4_51_codex_and_opencode_paths_need_the_host_dir() {
        let dir = TempDir::new("host-dirs");
        let (home, root) = (dir.path.join("home"), dir.path.join("repo"));
        assert_eq!(codex_mcp_path(&home), None);
        assert_eq!(opencode_mcp_path(&root, &home), None);
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(home.join(".config").join("opencode")).unwrap();
        assert_eq!(
            codex_mcp_path(&home),
            Some(home.join(".codex").join("config.toml"))
        );
        assert_eq!(
            opencode_mcp_path(&root, &home),
            Some(root.join("opencode.json"))
        );
    }

    /// The action word per writer: the section upsert says `appended`
    /// for a file with no block; the JSON merge, the Codex TOML upsert and
    /// the hook writers say `updated` for a file that existed.
    #[test]
    fn test_p4_52_action_words_follow_golden() {
        let dir = TempDir::new("action-words");
        let p = |name: &str| dir.path.join(name);

        fs::write(p("AGENTS.md"), "# My rules\n").unwrap();
        assert_eq!(
            upsert_section(&p("AGENTS.md"), "one").unwrap(),
            WriteAction::Appended
        );
        assert_eq!(
            upsert_section(&p("AGENTS.md"), "one").unwrap(),
            WriteAction::Unchanged
        );
        assert_eq!(
            upsert_section(&p("AGENTS.md"), "two").unwrap(),
            WriteAction::Replaced
        );

        fs::write(
            p("mcp.json"),
            r#"{"mcpServers":{"mine":{"command":"foo"}}}"#,
        )
        .unwrap();
        assert_eq!(
            merge_mcp_json(&p("mcp.json"), "mcpServers").unwrap(),
            WriteAction::Updated
        );

        fs::write(p("hooks.json"), r#"{"hooks":{"Stop":[]}}"#).unwrap();
        assert_eq!(
            merge_codex_hooks(&p("hooks.json")).unwrap(),
            WriteAction::Updated
        );

        fs::write(p("cursor.json"), "{}").unwrap();
        assert_eq!(
            write_cursor_hooks(&p("cursor.json")).unwrap(),
            WriteAction::Updated
        );
        assert_eq!(WriteAction::Appended.word(), "appended");
        assert_eq!(WriteAction::Updated.word(), "updated");
    }

    /// Both ignore blocks, as `build` writes them, after a user's line.
    #[test]
    fn test_p1_72_strip_ignore_files_remove_only_the_whole_line_block() {
        let dir = TempDir::new("ignore-blocks");
        let gi = dir.path.join(".gitignore");
        let ig = dir.path.join(".ignore");
        let gblock = sieve_core::ignore::gitignore_block("sieve");
        let iblock = sieve_core::ignore::ignore_block("sieve");
        // A line that holds the block's first line inside other text stays.
        let user = "node_modules/\nx# sieve's local graph cache\n";
        fs::write(&gi, format!("{user}\n{gblock}")).unwrap();
        fs::write(&ig, format!("mine\n\n{iblock}")).unwrap();
        assert_eq!(strip_gitignore(&gi, false).unwrap(), Retract::Removed);
        assert_eq!(strip_gitignore(&gi, true).unwrap(), Retract::Removed);
        assert_eq!(fs::read_to_string(&gi).unwrap(), user);
        assert_eq!(strip_ignore(&ig, true).unwrap(), Retract::Removed);
        assert_eq!(fs::read_to_string(&ig).unwrap(), "mine\n");
        // The user's own `/sieve/` line without our comment stays.
        fs::write(&gi, "/sieve/\n").unwrap();
        assert_eq!(strip_gitignore(&gi, true).unwrap(), Retract::Absent);
        // A file with only the block goes.
        fs::write(&ig, &iblock).unwrap();
        assert_eq!(strip_ignore(&ig, true).unwrap(), Retract::Deleted);
        assert!(!ig.exists());
    }

    const GOLDEN_TABLE: &str = "[mcp_servers.sieve]\ncommand = \"sieve\"\nargs = [\"mcp\"]\n";

    /// The expected strip result: the table goes and every other byte stays: the
    /// `uninstall -y` result of each file below.
    #[test]
    fn test_p1_72_strip_mcp_toml_keeps_user_bytes_as_golden() {
        let dir = TempDir::new("toml-bytes");
        let path = dir.path.join("config.toml");
        // (file before sieve init, the init adds `\n` + table, the strip result)
        let cases: [(&str, &str); 6] = [
            (
                "[mcp_servers.a]\r\ncommand = \"x\"\r\n\r\n[other]\r\nx = 1\r\n",
                "crlf",
            ),
            ("[other]\nx = 1", "no trailing newline"),
            ("  # indented\n    [mcp_servers.a]\nx=1\n", "indent"),
            ("\u{feff}[other]\nx = 1\n", "bom"),
            ("# top\n[other]\nx = 1\n", "comment first"),
            ("", "empty file"),
        ];
        for (orig, label) in cases {
            let before = format!(
                "{orig}{}{GOLDEN_TABLE}",
                if orig.ends_with('\n') || orig.is_empty() {
                    "\n"
                } else {
                    "\n\n"
                }
            );
            let before = if orig.is_empty() {
                GOLDEN_TABLE.to_string()
            } else {
                before
            };
            fs::write(&path, &before).unwrap();
            let r = strip_mcp_toml(&path, true).unwrap();
            if orig.is_empty() {
                assert_eq!(r, Retract::Deleted, "{label}");
                continue;
            }
            // The strip adds the final newline the user's file lacked.
            let want = if orig.ends_with('\n') {
                orig.to_string()
            } else {
                format!("{orig}\n")
            };
            assert_eq!(fs::read_to_string(&path).unwrap(), want, "{label}");
        }
        // The one other change: 3 newlines or more fold to 2.
        fs::write(&path, format!("[a]\nx=1\n\n\n\n[b]\ny=2\n\n{GOLDEN_TABLE}")).unwrap();
        strip_mcp_toml(&path, true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "[a]\nx=1\n\n[b]\ny=2\n");
        // A table in the middle keeps the tables after it; a BOM header is found.
        fs::write(&path, format!("\u{feff}{GOLDEN_TABLE}[o]\nx=1\n")).unwrap();
        assert_eq!(strip_mcp_toml(&path, true).unwrap(), Retract::Removed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[o]\nx=1\n");
    }

    #[test]
    fn test_p1_72_merge_mcp_toml_finds_a_bom_header_and_adds_no_second_table() {
        let dir = TempDir::new("toml-bom");
        let path = dir.path.join("config.toml");
        fs::write(&path, format!("\u{feff}{}", golden_toml_table())).unwrap();
        assert_eq!(merge_mcp_toml(&path).unwrap(), WriteAction::Unchanged);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("[mcp_servers.sieve]").count(), 1);
    }

    /// An inline or dotted `mcp_servers.<product>` key would make a second
    /// table invalid TOML. A plain append would add it; Sieve leaves the file (P4-51
    /// deviation).
    #[test]
    fn test_p4_51_merge_mcp_toml_skips_an_inline_or_dotted_server() {
        let dir = TempDir::new("toml-inline");
        let path = dir.path.join("config.toml");
        for text in [
            "mcp_servers = { sieve = { command = \"u\" } }\n",
            "mcp_servers.sieve.command = \"u\"\n",
            "[mcp_servers]\nsieve = { command = \"u\" }\n",
            "[mcp_servers]\nsieve.command = \"u\"\n",
        ] {
            fs::write(&path, text).unwrap();
            assert_eq!(
                merge_mcp_toml(&path).unwrap(),
                WriteAction::SkippedUnparseable,
                "{text}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
        // Another server in the same forms is no reason to skip.
        fs::write(&path, "mcp_servers.other.command = \"u\"\n").unwrap();
        assert_eq!(merge_mcp_toml(&path).unwrap(), WriteAction::Updated);
    }

    /// The `uninstall -y` result for each `AGENTS.md` (captured), after
    /// `init` appended the block to the user's text.
    #[test]
    fn test_p1_72_strip_section_keeps_user_bytes_as_golden() {
        let dir = TempDir::new("section-bytes");
        let path = dir.path.join("AGENTS.md");
        let block = format!("{}\nx\n{}", section_start(), section_end());
        let cases = [
            ("a\r\n\r\nb\r\n", "\r\n", "a\r\n\r\nb\r\n"),
            ("a\nb", "\n", "a\nb\n"),
            ("  # hi\n\nx", "\n", "  # hi\n\nx\n"),
            ("a\n\n\n\nb\n", "\n", "a\n\nb\n"),
            ("\u{feff}head\n", "\n", "\u{feff}head\n"),
        ];
        for (orig, eol, want) in cases {
            let sep = if orig.ends_with(eol) {
                eol.to_string()
            } else {
                eol.repeat(2)
            };
            let block = block.replace('\n', eol);
            fs::write(&path, format!("{orig}{sep}{block}{eol}")).unwrap();
            assert_eq!(strip_section(&path, true).unwrap(), Retract::Removed);
            assert_eq!(fs::read_to_string(&path).unwrap(), want, "{orig:?}");
        }
        // An unclosed start marker is no block: the file stays.
        let text = format!("a\n{}\nuser text\n", section_start());
        fs::write(&path, &text).unwrap();
        assert_eq!(strip_section(&path, true).unwrap(), Retract::Absent);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    /// Only the blank line `ensure_entry` added goes, and the user's final
    /// newline stays.
    #[test]
    fn test_p1_72_strip_ignore_keeps_user_blank_lines_and_final_newline() {
        let dir = TempDir::new("ignore-blank");
        let path = dir.path.join(".gitignore");
        let block = sieve_core::ignore::gitignore_block("sieve");
        // The user's own blank line before ours stays (one blank was ours).
        fs::write(&path, format!("a\n\n\n{block}")).unwrap();
        strip_gitignore(&path, true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\n\n");
        // No final newline after the block: the newline after `a` stays.
        fs::write(&path, format!("a\n\n{}", block.trim_end())).unwrap();
        strip_gitignore(&path, true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\n");
        // No blank line before the block: none is removed.
        fs::write(&path, format!("a\n{block}b\n")).unwrap();
        strip_gitignore(&path, true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\n");
    }

    #[test]
    fn strip_gitignore_deletes_a_file_that_holds_only_sieve_block() {
        let dir = TempDir::new("gitignore");
        let path = dir.path.join(".gitignore");
        fs::write(
            &path,
            "# sieve's local graph cache — regenerable, not committed (run `sieve build`).\n/sieve/\n",
        )
        .unwrap();
        assert!(strip_gitignore(&path, true).unwrap().hit());
        assert!(!path.exists());
    }

    #[test]
    fn test_f3_registers_pre_tool_use() {
        let hooks = merged_hooks_ojson(None);
        let entries = hooks
            .get("PreToolUse")
            .and_then(OJson::as_arr)
            .expect("the merge must register a PreToolUse key");
        assert!(
            entries
                .iter()
                .any(|e| e.get("matcher").and_then(OJson::as_str) == Some("Read")),
            "the PreToolUse block must carry the Read matcher, got: {entries:?}"
        );
    }

    /// Every `PostToolUse` hook command in the merged object.
    fn post_tool_use_commands(hooks: &OJson) -> Vec<(String, String)> {
        hooks
            .get("PostToolUse")
            .and_then(OJson::as_arr)
            .expect("a PostToolUse key")
            .iter()
            .flat_map(|e| {
                let matcher = e.get("matcher").and_then(OJson::as_str).unwrap_or("");
                e.get("hooks")
                    .and_then(OJson::as_arr)
                    .expect("hooks")
                    .iter()
                    .map(|h| {
                        let command = h.get("command").and_then(OJson::as_str).unwrap_or("");
                        (matcher.to_string(), command.to_string())
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Every `PreToolUse` matcher in the merged object.
    fn pre_tool_use_matchers(hooks: &OJson) -> Vec<String> {
        hooks
            .get("PreToolUse")
            .and_then(OJson::as_arr)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.get("matcher").and_then(OJson::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn test_f1_registers_bash_on_both_events() {
        let hooks = merged_hooks_ojson(None);
        let commands = post_tool_use_commands(&hooks);
        assert!(
            commands
                .iter()
                .any(|(m, c)| m == "Bash" && c.ends_with("hook post-read")),
            "the merge must register post-read on the Bash matcher, got: {commands:?}"
        );
        let matchers = pre_tool_use_matchers(&hooks);
        assert!(
            matchers.iter().any(|m| m == "Bash"),
            "the merge must register pre-read on the Bash matcher, got: {matchers:?}"
        );
    }

    #[test]
    fn test_f1_registers_post_tool_use() {
        let hooks = merged_hooks_ojson(None);
        let commands = post_tool_use_commands(&hooks);
        assert!(
            commands
                .iter()
                .any(|(m, c)| m == "Read" && c.ends_with("hook post-read")),
            "the merge must register post-read on the Read matcher, got: {commands:?}"
        );
    }
}
