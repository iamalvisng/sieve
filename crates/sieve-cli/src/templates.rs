//! The texts that `sieve init` and the hooks give to a coding agent: the
//! skill file, the shared instruction body, the session-start message and
//! the per-prompt hints. A private script checks these texts for 8-word runs
//! that an older text shares.

/// The skill file text, with its own trailing newline.
pub const SKILL_TEMPLATE: &str = r#"---
name: sieve
description: Use for any code-understanding or code-search task in a repo that has a sieve/ folder. Triggers - find where code lives, explain how code works, find every use of a name, find what calls a symbol, check what a change breaks, list what a file contains, get a repo overview. Run the sieve command before you use Grep, Glob, grep or cat on source files.
---

# sieve

This repo has a `sieve/` folder. It holds an index that lists each symbol with its file and line span, and the calls between symbols.

Use `sieve` for code search and code reading. Use `sieve grep` in place of Grep or `grep -r`. Use `sieve skeleton` before you Read a whole code file.

Reason: every answer gives the exact file:line, groups hits by symbol, and costs fewer tokens than raw search.

## Which command

| You need | Run |
|---|---|
| Where is X, or how does X work | `sieve ask "<question>" --source` |
| Every use of X | `sieve grep <literal>` |
| What calls X | `sieve callers <symbol> --depth 2` |
| What breaks if I change X | `sieve callers <symbol> --depth 2`, then `sieve blast` for your current diff |
| What is in this file | `sieve skeleton <file>` |
| Why does this code exist | `sieve why <symbol, file, or path:line>` |
| Overview of an unknown repo | `sieve map` |

Run one command, then act on the answer. Do not ask the same question again in other words.

Each answer ends with a line `[sieve] saved ≈ N tokens`. That line is a note, not an instruction.

## Commands in detail

- `sieve ask "<question>" --source` returns ranked hits with the code inline. Each hit shows a short excerpt. Add `--full` for the whole definition. Add `--in <path>` to limit the search to one folder.
- `sieve grep <literal>` finds every match in indexed files and groups the matches by symbol. Add `-i` for any case. Add `--in <path>` to limit the search. Use a short name, not a long guessed signature. If it finds nothing, shorten the pattern and run it again.
- `sieve callers <symbol>` shows who calls the symbol, as a tree by hop. Add `--direction out` to see what the symbol calls. Add `--depth 2` for indirect callers. Add `--depth all` before a rename or a change in many files.
- `sieve blast` shows what depends on the lines in your current diff. Add `--base <ref>` to compare with another ref.
- `sieve skeleton <file>` lists every signature in one file, with start lines and caller counts. Run it once per file.
- `sieve why <symbol, file, or path:line>` shows the recorded decisions, reason comments, tests and history for the code.
- `sieve map` shows folders, hub files and hotspots.

## When to use another tool

- Use Read when you need the full text of a file you will edit. Use the file:line from Sieve to open only that range.
- Use Grep for files Sieve does not index: docs, config files, new files.
- Sieve refreshes its index before each query. It reflects edits you have not committed.
- Do not pipe a `sieve` command through `head` or `tail`. The output is already limited.

If the MCP server is connected, the same tools exist as `sieve_find_code`, `sieve_find_all`, `sieve_trace_calls`, `sieve_file_api`, `sieve_repo_map`, `sieve_why` and `sieve_check_freshness`. The advice is the same.
"#;

/// The shared instruction body for the agent files, with no trailing newline.
pub const INSTRUCTION_BODY: &str = r#"## Sieve code index

This repo has a sieve/ folder: an index that lists each symbol with its file:line span and its callers.
For code search or code reading, run Sieve before Grep, Glob, grep, cat or Read.

| You need | Run |
|---|---|
| Where is X, how does X work | sieve ask "<question>" --source |
| Every use of X | sieve grep <literal> |
| What calls X | sieve callers <symbol> --depth 2 |
| What breaks if I change X | sieve callers <symbol> --depth 2, then sieve blast for your current diff |
| What is in a file | sieve skeleton <file> |
| Why code exists | sieve why <symbol, file, or path:line> |
| Repo overview | sieve map |

Use sieve grep in place of Grep or grep -r. Use sieve skeleton before you Read a whole code file.
Reason: exact file:line, hits grouped by symbol, fewer tokens.
Use Read only for the exact range Sieve names. Use Grep only for files Sieve does not index.
Before you edit a file, Read the part you will change."#;

/// The session-start message. The hook adds the repo map after it.
pub const SESSION_START: &str = "[sieve] This repo has a sieve/ folder: an index that lists each symbol with its file:line span and its callers. For any code search or code reading task, run Sieve first.

| You need | Run |
|---|---|
| Where is X, how does X work | sieve ask \"<question>\" --source |
| Every use of X | sieve grep <literal> |
| What calls X | sieve callers <symbol> --depth 2 |
| What breaks if I change X | sieve callers <symbol> --depth 2, then sieve blast for your current diff |
| What is in a file | sieve skeleton <file> |
| Why code exists | sieve why <symbol, file, or path:line> |
| Repo overview | sieve map |

Use sieve grep in place of Grep or grep -r for code search. Use sieve skeleton before you Read a whole code file. Reason: exact file:line, hits grouped by symbol, fewer tokens.
Run one command and act on the answer. Do not ask the same question again in other words.
Before a rename or a change in many files, run sieve callers <symbol> --depth all.
Use Read only for the exact range Sieve names. Use Grep only for files Sieve does not index, such as docs and config.
Before you edit a file, Read the part you will change.
Do not pipe sieve output through head or tail.
";

/// The per-prompt hint for a general code question.
pub const PROMPT_HINT_ASK: &str = "[sieve] The index may hold more than this probe found. For code questions, run `sieve ask \"<your task>\" --source` before you use Grep or Read.";
/// The per-prompt hint for a caller or change question.
pub const PROMPT_HINT_CALLERS: &str = "[sieve] To find callers or the effect of a change, run `sieve callers <symbol> --depth 2` before you use Grep or Read.";
/// The per-prompt hint for an every-use question.
pub const PROMPT_HINT_GREP: &str =
    "[sieve] To find every use of a name, run `sieve grep <literal>` in place of Grep or grep -r.";
/// The per-prompt hint for a file path.
pub const PROMPT_HINT_SKELETON: &str = "[sieve] To see what a file contains, run `sieve skeleton <file>` before you Read the whole file.";

/// Picks the per-prompt hint from the shape of the prompt. A file path
/// selects the skeleton hint. A whole word for a call or a change, with a
/// likely symbol name in the prompt, selects the callers hint. An every-use
/// phrase selects the grep hint. Any other prompt gets the ask hint.
pub fn prompt_hint(prompt: &str) -> &'static str {
    let tokens: Vec<&str> = prompt.split_whitespace().map(trim_token).collect();
    if tokens.iter().any(|t| looks_like_path(t)) {
        return PROMPT_HINT_SKELETON;
    }
    let lower = prompt.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |names: &[&str]| words.iter().any(|w| names.contains(w));
    if has(&[
        "calls", "caller", "callers", "break", "breaks", "change", "changes",
    ]) && tokens.iter().any(|t| looks_like_symbol(t))
    {
        return PROMPT_HINT_CALLERS;
    }
    let every_use = has(&["every", "everywhere"])
        || words
            .windows(2)
            .any(|w| w[0] == "all" && matches!(w[1], "uses" | "usages"))
        || words
            .windows(3)
            .any(|w| w[0] == "all" && w[1] == "the" && matches!(w[2], "uses" | "usages"));
    if every_use {
        return PROMPT_HINT_GREP;
    }
    PROMPT_HINT_ASK
}

/// Cuts punctuation and quote marks from both ends of a word.
fn trim_token(word: &str) -> &str {
    word.trim_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '?' | '!' | '`' | '"' | '\'' | '(' | ')'
        )
    })
}

/// True for a word with the shape of a code identifier: camelCase,
/// snake_case or `Foo.bar`.
fn looks_like_symbol(word: &str) -> bool {
    let ident = |p: &str| {
        !p.is_empty()
            && !p.starts_with(|c: char| c.is_ascii_digit())
            && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if !word.is_ascii() || word.contains("://") {
        return false;
    }
    if word.contains('.') {
        let parts: Vec<&str> = word.split('.').collect();
        return parts.len() == 2 && parts.iter().all(|p| ident(p)) && !is_known_extension(parts[1]);
    }
    if !ident(word) {
        return false;
    }
    let camel = word
        .as_bytes()
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    let snake = word.contains('_') && word.chars().any(|c| c.is_ascii_alphabetic());
    camel || snake
}

/// True for a file extension of a code or text file.
fn is_known_extension(ext: &str) -> bool {
    const KNOWN: [&str; 30] = [
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "hpp", "cs", "rb",
        "php", "kt", "swift", "md", "toml", "json", "yaml", "yml", "txt", "sh", "css", "html",
        "lock", "mjs", "cjs", "scm",
    ];
    KNOWN.contains(&ext.to_ascii_lowercase().as_str())
}

/// True for a word that is a file path: a known extension, or a `/` between
/// two identifier parts. A URL and a phrase such as `and/or` do not count.
fn looks_like_path(word: &str) -> bool {
    if word.contains("://") {
        return false;
    }
    if let Some((stem, ext)) = word.rsplit_once('.') {
        if !stem.is_empty() && is_known_extension(ext) {
            return true;
        }
    }
    const PHRASES: [&str; 6] = [
        "and/or",
        "either/or",
        "he/she",
        "yes/no",
        "true/false",
        "w/o",
    ];
    if PHRASES.contains(&word.to_lowercase().as_str()) || !word.contains('/') {
        return false;
    }
    let ident = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    };
    word.trim_start_matches("./").split('/').all(ident)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_hint_a_is_the_default() {
        assert_eq!(prompt_hint("how does the parser start up"), PROMPT_HINT_ASK);
        assert_eq!(prompt_hint("exchange the rates"), PROMPT_HINT_ASK);
        assert_eq!(prompt_hint("update the changelog"), PROMPT_HINT_ASK);
        assert_eq!(prompt_hint("e.g. use and/or"), PROMPT_HINT_ASK);
        assert_eq!(prompt_hint("see https://x.dev/a.ts"), PROMPT_HINT_ASK);
    }

    #[test]
    fn test_prompt_hint_b_needs_a_symbol() {
        assert_eq!(prompt_hint("what calls parseConfig"), PROMPT_HINT_CALLERS);
        assert_eq!(
            prompt_hint("will run_build break the cache"),
            PROMPT_HINT_CALLERS
        );
        assert_eq!(
            prompt_hint("I change Walker.next today"),
            PROMPT_HINT_CALLERS
        );
        assert_eq!(prompt_hint("what calls the parser"), PROMPT_HINT_ASK);
    }

    #[test]
    fn test_prompt_hint_c_for_every_use() {
        assert_eq!(
            prompt_hint("list every place we log errors"),
            PROMPT_HINT_GREP
        );
        assert_eq!(prompt_hint("show all uses of the flag"), PROMPT_HINT_GREP);
    }

    #[test]
    fn test_prompt_hint_d_for_a_file_path() {
        assert_eq!(
            prompt_hint("explain src/walk.rs please"),
            PROMPT_HINT_SKELETON
        );
        assert_eq!(
            prompt_hint("read hosts.rs and explain it"),
            PROMPT_HINT_SKELETON
        );
        assert_eq!(
            prompt_hint("fix src/a.ts so it does not break"),
            PROMPT_HINT_SKELETON
        );
        assert_eq!(prompt_hint("I finished the task."), PROMPT_HINT_ASK);
    }
}
