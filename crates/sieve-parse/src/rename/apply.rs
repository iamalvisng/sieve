//! The write half of the rename: apply, journal, rollback, recover and the
//! diff printer.
//!
//! `apply_plan` never applies a stored plan. It plans again under the build
//! lock, checks every file, writes a journal, writes each file by temp file
//! and rename, rebuilds the graph and checks the result. A failed check
//! restores every file from the bytes in memory.
//!
//! Limit: a write keeps the file mode and, on unix, tries to keep the owner
//! and group. It does not keep extended attributes, because `std` has no API
//! for them. A temp-and-rename write also changes the inode and breaks hard
//! links (a rename rule).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sieve_core::Confidence;

use super::{
    decode_any, grammar_of, hex_sha256, parse, path_has_symlink, plan_counted, span_lines, Counts,
    Edit, Parsers, Plan, Reason, Refusal, RenameError, Status, JOURNAL_PATH,
};
use crate::refresh::{acquire_refresh_lock, rebuild_graph_only, rebuild_keeping, Kept};
use crate::ts_scope::{scope_file, word_matches, SiteKind};

/// How an apply call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    /// Every edit is on disk, the graph is fresh and the check passed.
    Written,
    /// The plan is refused, or a file failed the pre-write checks. No file
    /// changed.
    Refused,
    /// The new plan id differs from `--expect`. No file changed.
    ExpectMismatch,
    /// A write or the post-check failed. Every file is back to its old
    /// bytes, and `message` says why.
    RolledBack,
    /// A write or the post-check failed, and a file that someone else
    /// changed was left as it is. The journal is kept. `conflicts` lists the
    /// files, and `message` says what to do.
    RollbackConflict,
}

/// The result of one `apply_plan` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    /// How the call ended.
    pub status: ApplyStatus,
    /// The plan that the call computed under the lock.
    pub plan: Plan,
    /// The repo-relative paths that were written, or restored on a rollback.
    pub files: Vec<String>,
    /// A short reason for the reader. None after a clean write.
    pub message: Option<String>,
    /// The files that a rollback left alone, because they changed after the
    /// write. Empty unless the status is `RollbackConflict`.
    pub conflicts: Vec<String>,
}

/// The result of one `recover` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoverReport {
    /// True when a journal was found.
    pub found: bool,
    /// The repo-relative paths that were rewritten with their old bytes.
    pub restored: Vec<String>,
    /// The files whose bytes match neither the old nor the planned new
    /// bytes. `recover` leaves them alone and keeps the journal. Check them,
    /// then call `discard_journal`.
    pub conflicts: Vec<String>,
}

/// One file in the journal: the bytes before the rename, and the hashes of
/// the old and the planned new bytes.
#[derive(Debug, Serialize, Deserialize)]
struct JournalFile {
    path: String,
    sha256: String,
    new_sha256: String,
    bytes_hex: String,
}

/// The write journal, version 1 (the plan file for a crash).
#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u32,
    old: String,
    new: String,
    files: Vec<JournalFile>,
    /// The sha256 over all the fields above.
    checksum: String,
}

impl Journal {
    fn compute_checksum(&self) -> String {
        let mut hasher = Sha256::new();
        let mut put = |part: &str| {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        };
        put(&self.version.to_string());
        put(&self.old);
        put(&self.new);
        for file in &self.files {
            put(&file.path);
            put(&file.sha256);
            put(&file.new_sha256);
            put(&file.bytes_hex);
        }
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// The file attributes that a write keeps.
#[derive(Debug, Clone)]
pub(super) struct Attrs {
    perms: fs::Permissions,
    #[cfg(unix)]
    owner: (u32, u32),
}

fn attrs_of(meta: &fs::Metadata) -> Attrs {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Attrs {
            perms: meta.permissions(),
            owner: (meta.uid(), meta.gid()),
        }
    }
    #[cfg(not(unix))]
    {
        Attrs {
            perms: meta.permissions(),
        }
    }
}

/// True when the owner can write the file by its mode bits.
fn owner_can_write(perms: &fs::Permissions) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.mode() & 0o200 != 0
    }
    #[cfg(not(unix))]
    {
        !perms.readonly()
    }
}

/// One file that the rename will write.
#[derive(Debug)]
pub(super) struct Staged {
    path: String,
    old: Vec<u8>,
    new: Vec<u8>,
    attrs: Attrs,
}

/// Test seams. They exist only in test builds.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Hooks {
    /// Makes the post-check fail.
    #[cfg(test)]
    pub fail_post_check: bool,
    /// Makes the write of the file at this index fail.
    #[cfg(test)]
    pub fail_write_at: Option<usize>,
    /// Makes every restore fail.
    #[cfg(test)]
    pub fail_restore: bool,
    /// Returns an error before the file at this index is written, with the
    /// journal and the files left as they are (a crash).
    #[cfg(test)]
    pub crash_before: Option<usize>,
    /// Drops this path from the plan, as if the scan had skipped the file.
    #[cfg(test)]
    pub drop_file: Option<&'static str>,
    /// Changes this file after the write, as a racing editor would.
    #[cfg(test)]
    pub tamper_file: Option<&'static str>,
    /// Removes the edit at this index of this file from the plan.
    #[cfg(test)]
    pub skip_edit: Option<(&'static str, usize)>,
    /// Adds this file to the plan with an edit for every hit of the old
    /// name, as an over-edit of an unrelated symbol would.
    #[cfg(test)]
    pub add_file: Option<&'static str>,
    /// Changes this file inside the post-check, after check (a).
    #[cfg(test)]
    pub tamper_late: Option<&'static str>,
    /// Makes the final directory sync fail.
    #[cfg(test)]
    pub fail_sync: bool,
}

/// Applies the rename of `symbol` to `new_name`.
///
/// It plans again under the build lock. With `expect`, the new plan id must
/// equal it. It writes nothing when the plan is refused or a check fails. A
/// failed write or post-check restores every file and reports `RolledBack`.
pub fn apply_plan(
    root: &Path,
    context_dir: &Path,
    symbol: &str,
    new_name: &str,
    in_prefix: Option<&str>,
    expect: Option<&str>,
) -> Result<ApplyReport, RenameError> {
    apply_inner(
        root,
        context_dir,
        symbol,
        new_name,
        in_prefix,
        expect,
        Hooks::default(),
    )
}

pub(super) fn apply_inner(
    root: &Path,
    context_dir: &Path,
    symbol: &str,
    new_name: &str,
    in_prefix: Option<&str>,
    expect: Option<&str>,
    hooks: Hooks,
) -> Result<ApplyReport, RenameError> {
    fs::create_dir_all(context_dir)?;
    let lock = acquire_refresh_lock(context_dir)?.ok_or(RenameError::LockBusy)?;
    let result = apply_locked(
        root,
        context_dir,
        symbol,
        new_name,
        in_prefix,
        expect,
        hooks,
    );
    drop(lock);
    result
}

fn report(
    status: ApplyStatus,
    plan: Plan,
    files: Vec<String>,
    message: Option<String>,
) -> ApplyReport {
    ApplyReport {
        status,
        plan,
        files,
        message,
        conflicts: Vec::new(),
    }
}

/// The Extracted inbound edges of `id`, as (source id, relation) rows.
fn inbound(kept: &Kept, id: &str) -> BTreeSet<(String, String)> {
    kept.graph
        .edges
        .iter()
        .filter(|e| e.target == id && e.confidence == Confidence::Extracted)
        .map(|e| (e.source.clone(), format!("{:?}", e.relation)))
        .collect()
}

/// The inbound links that must survive the rename (check c). A link is
/// required when its raw edge carried an import specifier. A link without a
/// specifier can be a false edge from a property call such as `cfg.f()` or a
/// shadowed call (a rename rule). It is exempt only when its source node holds an
/// old-name hit that the plan classed Other for a structural reason. Any
/// other such link must reappear on the new node.
fn required_links(kept: &Kept, plan: &Plan, counts: &Counts) -> BTreeSet<(String, String)> {
    let id = &plan.target.id;
    let mut out = BTreeSet::new();
    for edge in kept
        .graph
        .edges
        .iter()
        .filter(|e| &e.target == id && e.confidence == Confidence::Extracted)
    {
        let bound = kept.raw.iter().any(|r| {
            r.source == edge.source
                && r.relation == edge.relation
                && r.specifier.is_some()
                && r.import_name.is_none()
                && r.name.as_deref() == Some(plan.old.as_str())
        });
        let structural = kept
            .graph
            .nodes
            .iter()
            .find(|n| n.id == edge.source)
            .is_some_and(|node| {
                let (first, last) = span_lines(&node.span);
                counts.structural.iter().any(|(path, line)| {
                    *path == node.path && (first as usize..=last as usize).contains(line)
                })
            });
        if bound || !structural {
            out.insert((edge.source.clone(), format!("{:?}", edge.relation)));
        }
    }
    out
}

/// What the plan step records for the post-check.
struct Expect {
    /// Check (c): the links that must survive.
    links: BTreeSet<(String, String)>,
    /// Check (f): every node id before the write.
    nodes: BTreeSet<String>,
    /// Check (e): the old-name hits in TS and JS files that stay.
    kept_hits: usize,
    /// Check (e): the new-name count before the write.
    new_before: usize,
    /// Check (e): the number of planned edits.
    edits: usize,
}

/// Counts whole-word hits of `old` and `new` in every claimed TS or JS file.
/// It reads the decoded text, as the plan pre-filter does.
fn word_counts(root: &Path, kept: &Kept, old: &str, new: &str) -> Result<(usize, usize), String> {
    let (mut old_n, mut new_n) = (0, 0);
    for path in kept.prints.keys().filter(|p| grammar_of(p).is_some()) {
        let bytes = fs::read(root.join(path)).map_err(|e| format!("{path}: {e}"))?;
        let text = decode_any(&bytes);
        old_n += word_matches(&text, old).count();
        new_n += word_matches(&text, new).count();
    }
    Ok((old_n, new_n))
}

fn apply_locked(
    root: &Path,
    context_dir: &Path,
    symbol: &str,
    new_name: &str,
    in_prefix: Option<&str>,
    expect: Option<&str>,
    hooks: Hooks,
) -> Result<ApplyReport, RenameError> {
    // Plan again. A stored plan is never applied.
    let kept = rebuild_keeping(root, context_dir)?;
    let (mut plan, counts) =
        plan_counted(root, context_dir, &kept, symbol, new_name, in_prefix, true)?;
    if plan.status == Status::Refused {
        let text = format!("the plan is refused: {} cause(s)", plan.refused.len());
        return Ok(report(ApplyStatus::Refused, plan, Vec::new(), Some(text)));
    }
    if let Some(want) = expect {
        if want != plan.plan_id {
            let text = format!("plan id {} does not match expected {want}", plan.plan_id);
            return Ok(report(
                ApplyStatus::ExpectMismatch,
                plan,
                Vec::new(),
                Some(text),
            ));
        }
    }
    let (_, new_before) =
        word_counts(root, &kept, &plan.old, &plan.new).map_err(RenameError::Apply)?;
    let want = Expect {
        links: required_links(&kept, &plan, &counts),
        nodes: kept.graph.nodes.iter().map(|n| n.id.clone()).collect(),
        kept_hits: counts.kept_hits,
        new_before,
        edits: plan.files.iter().map(|f| f.edits.len()).sum(),
    };
    drop(kept);
    #[cfg(test)]
    {
        if let Some(skip) = hooks.drop_file {
            plan.files.retain(|f| f.path != skip);
        }
        if let Some(path) = hooks.add_file {
            let bytes = fs::read(root.join(path))?;
            let text = decode_any(&bytes);
            let edits = word_matches(&text, &plan.old)
                .map(|at| {
                    let (line, col) = super::line_col(&text, at);
                    Edit {
                        start: at,
                        end: at + plan.old.len(),
                        line,
                        col,
                        site: super::SiteClass::Declaration,
                    }
                })
                .collect();
            plan.files.push(super::PlanFile {
                path: path.to_string(),
                sha256: hex_sha256(&bytes),
                edits,
            });
        }
        if let Some((path, index)) = hooks.skip_edit {
            for file in plan.files.iter_mut().filter(|f| f.path == path) {
                if index < file.edits.len() {
                    file.edits.remove(index);
                }
            }
        }
    }
    // Check every file before the first write.
    let staged = match precheck(root, &plan) {
        Ok(staged) => staged,
        Err(refusals) => {
            plan.status = Status::Refused;
            plan.files.clear();
            plan.refused = refusals;
            let text = "a file failed the post-write checks".to_string();
            return Ok(report(ApplyStatus::Refused, plan, Vec::new(), Some(text)));
        }
    };
    // Write the journal, before any write. A sync failure stops here.
    write_journal(context_dir, &plan, &staged)?;
    // Write each file by temp file and rename.
    for (i, item) in staged.iter().enumerate() {
        touch_lock(context_dir);
        #[cfg(test)]
        if hooks.crash_before == Some(i) {
            return Err(RenameError::Apply("simulated crash".to_string()));
        }
        #[cfg(test)]
        let wrote = if hooks.fail_write_at == Some(i) {
            Err(io::Error::other("forced write failure"))
        } else {
            write_atomic(&root.join(&item.path), &item.new, Some(&item.attrs))
        };
        #[cfg(not(test))]
        let wrote = write_atomic(&root.join(&item.path), &item.new, Some(&item.attrs));
        if let Err(err) = wrote {
            // The failed file is restored too: its rename may have run
            // before a later sync failed.
            let why = format!("the write of {} failed: {err}", item.path);
            return roll_back(root, context_dir, plan, &staged[..=i], &why, hooks);
        }
    }
    #[cfg(test)]
    {
        if hooks.crash_before == Some(staged.len()) {
            return Err(RenameError::Apply("simulated crash".to_string()));
        }
        if let Some(path) = hooks.tamper_file {
            let mut file = fs::OpenOptions::new().append(true).open(root.join(path))?;
            file.write_all(b"\n// tampered\n")?;
        }
    }
    // Rebuild, then post-check.
    if let Err(why) = post_check(root, context_dir, &plan, &staged, &want, hooks) {
        let why = format!("post-check failed: {why}");
        return roll_back(root, context_dir, plan, &staged, &why, hooks);
    }
    // Sync the touched directories again. A failure keeps the journal.
    #[cfg(test)]
    let synced = if hooks.fail_sync {
        Err(io::Error::other("forced sync failure"))
    } else {
        sync_touched(root, &staged)
    };
    #[cfg(not(test))]
    let synced = sync_touched(root, &staged);
    if let Err(err) = synced {
        let journal = context_dir.join(JOURNAL_PATH);
        let shown = journal.strip_prefix(root).unwrap_or(&journal);
        return Err(RenameError::Apply(format!(
            "the rename is complete on disk and passed its check, \
             but a directory sync failed: {err}. \
             The journal {} is kept only for safety. \
             Next step: run `sieve rename --discard-journal` to keep the rename, \
             or `sieve rename --recover` to undo it.",
            shown.display()
        )));
    }
    // The journal goes away only after a passed check.
    remove_journal(context_dir)?;
    let paths = staged.iter().map(|s| s.path.clone()).collect();
    Ok(report(ApplyStatus::Written, plan, paths, None))
}

/// Syncs the directory of each staged file.
fn sync_touched(root: &Path, staged: &[Staged]) -> io::Result<()> {
    let dirs: BTreeSet<PathBuf> = staged
        .iter()
        .filter_map(|s| root.join(&s.path).parent().map(Path::to_path_buf))
        .collect();
    dirs.iter().try_for_each(|d| sync_dir(d))
}

/// Restores `staged` from memory, removes the journal and rebuilds the
/// graph. It reads each file first. A file that holds the staged new bytes
/// is restored, and a file that already holds the old bytes needs nothing.
/// Any other file changed after the write, so the rollback leaves it alone,
/// lists it as a conflict and keeps the journal. A failed restore keeps the
/// journal and returns an error that names it.
fn roll_back(
    root: &Path,
    context_dir: &Path,
    plan: Plan,
    staged: &[Staged],
    why: &str,
    hooks: Hooks,
) -> Result<ApplyReport, RenameError> {
    let journal = context_dir.join(JOURNAL_PATH);
    let mut todo: Vec<&Staged> = Vec::new();
    let mut skip: Vec<String> = Vec::new();
    for item in staged {
        match fs::read(root.join(&item.path)) {
            Ok(now) if now == item.new => todo.push(item),
            Ok(now) if now == item.old => {}
            _ => skip.push(item.path.clone()),
        }
    }
    if let Err(files) = restore(root, &todo, hooks) {
        return Err(RenameError::RestoreFailed {
            why: why.to_string(),
            files,
            journal,
        });
    }
    let paths: Vec<String> = todo.iter().map(|s| s.path.clone()).collect();
    let rebuilt = rebuild_graph_only(root, context_dir);
    if !skip.is_empty() {
        let shown = journal.strip_prefix(root).unwrap_or(&journal);
        let text = format!(
            "rolled back, but {} changed after the write and was left as it is; \
             the journal {} is kept. Check the file, then run recover or discard the journal. {why}",
            skip.join(", "),
            shown.display()
        );
        let mut out = report(ApplyStatus::RollbackConflict, plan, paths, Some(text));
        out.conflicts = skip;
        return Ok(out);
    }
    remove_journal(context_dir)?;
    let text = match rebuilt {
        Ok(_) => format!("rolled back: {why}"),
        Err(err) => format!("rolled back: {why}; the rebuild after the rollback failed: {err}"),
    };
    Ok(report(ApplyStatus::RolledBack, plan, paths, Some(text)))
}

/// The pre-write check: the bytes at each span equal the old name, the
/// disk hash equals the plan, the file is writable and no touched path is a
/// symlink. It returns the staged files, or one refusal per failed file.
pub(super) fn precheck(root: &Path, plan: &Plan) -> Result<Vec<Staged>, Vec<Refusal>> {
    let mut staged = Vec::new();
    let mut refusals = Vec::new();
    for file in &plan.files {
        let line = file.edits.first().map_or(1, |e| e.line);
        let col = file.edits.first().map_or(1, |e| e.col);
        let mut refuse = |reason: Reason, text: &str| {
            refusals.push(Refusal {
                path: file.path.clone(),
                line,
                col,
                reason,
                text: text.to_string(),
            });
        };
        if path_has_symlink(root, &file.path) {
            refuse(
                Reason::UnsupportedSite,
                "the path or an ancestor is a symlink",
            );
            continue;
        }
        let full = root.join(&file.path);
        let read = fs::read(&full).and_then(|b| fs::metadata(&full).map(|m| (b, m)));
        let Ok((old, meta)) = read else {
            refuse(Reason::StaleIndex, "the file cannot be read");
            continue;
        };
        if !owner_can_write(&meta.permissions()) {
            refuse(Reason::UnsupportedSite, "the file is read-only");
            continue;
        }
        if hex_sha256(&old) != file.sha256 {
            refuse(Reason::StaleIndex, "the file changed since the plan");
            continue;
        }
        let Some(new) = splice(&old, &file.edits, plan.old.as_bytes(), plan.new.as_bytes()) else {
            refuse(
                Reason::StaleIndex,
                "an edit span does not hold the old name",
            );
            continue;
        };
        staged.push(Staged {
            path: file.path.clone(),
            old,
            new,
            attrs: attrs_of(&meta),
        });
    }
    if refusals.is_empty() {
        Ok(staged)
    } else {
        Err(refusals)
    }
}

/// Swaps the bytes of each edit span for `new`. It returns None when a span
/// is out of range, overlaps the last one, or does not hold `old`.
fn splice(bytes: &[u8], edits: &[Edit], old: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    for edit in edits {
        if edit.start < at || bytes.get(edit.start..edit.end)? != old {
            return None;
        }
        out.extend_from_slice(&bytes[at..edit.start]);
        out.extend_from_slice(new);
        at = edit.end;
    }
    out.extend_from_slice(&bytes[at..]);
    Some(out)
}

/// The post-write check. The checks use the graph and the files, not a second
/// plan over the repo:
/// (a) each written file holds exactly the staged bytes;
/// (b) the old node id is gone, and the new node sits on the same line;
/// (c) the Extracted inbound edges of the new node hold every required
///     inbound edge of the old node from before the write;
/// (d) no code site of the old name remains in a touched file;
/// (e) the old name appears in the claimed TS and JS files exactly as often
///     as the plan's Other and Report hits, and the new name appears as
///     often as before plus the edits.
fn post_check(
    root: &Path,
    context_dir: &Path,
    plan: &Plan,
    staged: &[Staged],
    want: &Expect,
    hooks: Hooks,
) -> Result<(), String> {
    let mut changed = Vec::new();
    for item in staged {
        match fs::read(root.join(&item.path)) {
            Ok(now) if now == item.new => {}
            _ => changed.push(item.path.clone()),
        }
    }
    if !changed.is_empty() {
        return Err(format!("{} changed after the write", changed.join(", ")));
    }
    #[cfg(test)]
    if let Some(path) = hooks.tamper_late {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(root.join(path))
            .map_err(|e| e.to_string())?;
        file.write_all(b"\n// late\n").map_err(|e| e.to_string())?;
    }
    let kept = rebuild_keeping(root, context_dir).map_err(|e| format!("rebuild failed: {e}"))?;
    #[cfg(test)]
    if hooks.fail_post_check {
        return Err("forced post-check failure".to_string());
    }
    #[cfg(not(test))]
    let _ = hooks;
    let old_id = &plan.target.id;
    let new_id = format!("{}#{}", plan.target.path, plan.new);
    if kept.graph.nodes.iter().any(|n| &n.id == old_id) {
        return Err(format!("{old_id} still exists after the write"));
    }
    let Some(node) = kept.graph.nodes.iter().find(|n| n.id == new_id) else {
        return Err(format!("{new_id} does not exist after the write"));
    };
    if span_lines(&node.span).0 != plan.target.line {
        return Err(format!("{new_id} moved to another line"));
    }
    // Check (f): the node set changes only by the rename of the target.
    let now: BTreeSet<&String> = kept.graph.nodes.iter().map(|n| &n.id).collect();
    if let Some(gone) = want
        .nodes
        .iter()
        .find(|id| *id != old_id && !now.contains(id))
    {
        return Err(format!("the node {gone} is gone after the write"));
    }
    if let Some(extra) = now
        .iter()
        .find(|id| **id != &new_id && !want.nodes.contains(**id))
    {
        return Err(format!("the node {extra} is new after the write"));
    }
    let after = inbound(&kept, &new_id);
    for (source, relation) in &want.links {
        let source = if source == old_id { &new_id } else { source };
        if !after.contains(&(source.clone(), relation.clone())) {
            return Err(format!(
                "the {relation} link from {source} to {new_id} is lost"
            ));
        }
    }
    let paths: Vec<&str> = staged.iter().map(|s| s.path.as_str()).collect();
    no_old_sites(root, &paths, &plan.old)?;
    let (old_n, new_n) = word_counts(root, &kept, &plan.old, &plan.new)?;
    if old_n != want.kept_hits {
        return Err(format!(
            "the old name {} appears {old_n} time(s) in TS and JS files, \
             but the plan keeps {} hit(s)",
            plan.old, want.kept_hits
        ));
    }
    if new_n != want.new_before + want.edits {
        return Err(format!(
            "the new name {} appears {new_n} time(s), expected {}",
            plan.new,
            want.new_before + want.edits
        ));
    }
    Ok(())
}

/// Check (d): scans only the given files. Any code site of `old` that is
/// left (a declaration, reference, call, JSX tag, import, export or
/// re-export) fails.
pub(super) fn no_old_sites(root: &Path, paths: &[&str], old: &str) -> Result<(), String> {
    let mut parsers = Parsers::new().map_err(|e| e.to_string())?;
    for path in paths {
        let Some(grammar) = grammar_of(path) else {
            continue;
        };
        let bytes = fs::read(root.join(path)).map_err(|e| format!("{path}: {e}"))?;
        let text = decode_any(&bytes);
        let tree = parse(&mut parsers, grammar, &text).ok_or(format!("{path}: parse failed"))?;
        let scopes = scope_file(&tree, &text, old);
        let left = scopes.sites.iter().find(|s| {
            matches!(
                s.kind,
                SiteKind::Declaration
                    | SiteKind::Reference
                    | SiteKind::Call
                    | SiteKind::JsxOpen
                    | SiteKind::JsxClose
                    | SiteKind::ImportSpecifier
                    | SiteKind::ExportClause
                    | SiteKind::ReExportSpecifier
            )
        });
        if let Some(site) = left {
            return Err(format!(
                "{path}:{}:{} the old name {old} remains",
                site.line, site.col
            ));
        }
    }
    Ok(())
}

// ---- files ----

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Syncs a directory entry list to disk. It does nothing off unix.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Creates a new temp file next to `path`, with a unique name. On unix the
/// file starts with the target mode (before the umask), so it is never more
/// open than the target.
fn create_temp(path: &Path, attrs: Option<&Attrs>) -> io::Result<(fs::File, PathBuf)> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for _ in 0..32 {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_file_name(format!(
            ".{name}.{}-{nanos:x}-{n}.sieve-tmp",
            std::process::id()
        ));
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            if let Some(attrs) = attrs {
                opts.mode(attrs.perms.mode() & 0o777);
            }
        }
        #[cfg(not(unix))]
        let _ = attrs;
        match opts.open(&tmp) {
            Ok(file) => return Ok((file, tmp)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("no free temp file name"))
}

fn fill_temp(file: &mut fs::File, bytes: &[u8], attrs: Option<&Attrs>) -> io::Result<()> {
    file.write_all(bytes)?;
    if let Some(attrs) = attrs {
        #[cfg(unix)]
        {
            // Best effort: only a privileged user can give a file away.
            let _ = std::os::unix::fs::fchown(&*file, Some(attrs.owner.0), Some(attrs.owner.1));
        }
        file.set_permissions(attrs.perms.clone())?;
    }
    file.sync_all()
}

/// Writes `bytes` to `path` by a new temp file in the same directory and a
/// rename, and syncs the file and the directory. It checks right before the
/// rename that `path` is not a symlink.
fn write_atomic(path: &Path, bytes: &[u8], attrs: Option<&Attrs>) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("the path has no parent"))?;
    let (mut file, tmp) = create_temp(path, attrs)?;
    let filled = fill_temp(&mut file, bytes, attrs);
    drop(file);
    let moved = filled.and_then(|()| {
        let linked = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
        if linked {
            Err(io::Error::other("the target became a symlink"))
        } else {
            fs::rename(&tmp, path)
        }
    });
    if moved.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    moved?;
    sync_dir(dir)
}

/// Writes the old bytes of each staged file back. It tries every file and
/// returns the paths that failed.
fn restore(root: &Path, staged: &[&Staged], hooks: Hooks) -> Result<(), Vec<String>> {
    let mut failed = Vec::new();
    for item in staged {
        #[cfg(test)]
        if hooks.fail_restore {
            failed.push(item.path.clone());
            continue;
        }
        #[cfg(not(test))]
        let _ = hooks;
        if write_atomic(&root.join(&item.path), &item.old, Some(&item.attrs)).is_err() {
            failed.push(item.path.clone());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed)
    }
}

/// Touches the lock file, so a long write does not look stale. A failure is ignored.
fn touch_lock(context_dir: &Path) {
    let path = sieve_core::lock::lock_path(context_dir);
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

// ---- journal ----

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// Builds the sealed journal for `(path, old bytes, new bytes)` rows.
fn make_journal(old: &str, new: &str, rows: &[(&str, &[u8], &[u8])]) -> Journal {
    let mut journal = Journal {
        version: 1,
        old: old.to_string(),
        new: new.to_string(),
        files: rows
            .iter()
            .map(|(path, before, after)| JournalFile {
                path: (*path).to_string(),
                sha256: hex_sha256(before),
                new_sha256: hex_sha256(after),
                bytes_hex: hex_encode(before),
            })
            .collect(),
        checksum: String::new(),
    };
    journal.checksum = journal.compute_checksum();
    journal
}

/// Writes the journal file, syncs it and its directory. Any failure stops
/// the apply before the first source write.
fn put_journal(context_dir: &Path, journal: &Journal) -> Result<(), RenameError> {
    let body = serde_json::to_vec(journal).map_err(|e| RenameError::Apply(e.to_string()))?;
    let path = context_dir.join(JOURNAL_PATH);
    let dir = path
        .parent()
        .ok_or_else(|| RenameError::Apply("the journal path has no parent".to_string()))?;
    let tmp = path.with_extension("json.tmp");
    let written = (|| -> io::Result<()> {
        fs::create_dir_all(dir)?;
        // A new `.cache` dir must reach the disk with its parent.
        if let Some(parent) = dir.parent() {
            sync_dir(parent)?;
        }
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &path)?;
        sync_dir(dir)
    })();
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        let _ = fs::remove_file(&path);
        return Err(RenameError::Io(err));
    }
    Ok(())
}

fn write_journal(context_dir: &Path, plan: &Plan, staged: &[Staged]) -> Result<(), RenameError> {
    let rows: Vec<(&str, &[u8], &[u8])> = staged
        .iter()
        .map(|s| (s.path.as_str(), s.old.as_slice(), s.new.as_slice()))
        .collect();
    put_journal(context_dir, &make_journal(&plan.old, &plan.new, &rows))
}

/// Writes a sealed journal from raw rows, with no path checks. Tests use it
/// to build journals that the real apply never writes.
#[cfg(test)]
pub(super) fn write_raw_journal(
    context_dir: &Path,
    rows: &[(&str, &[u8], &[u8])],
) -> Result<(), RenameError> {
    put_journal(context_dir, &make_journal("f", "g", rows))
}

fn remove_journal(context_dir: &Path) -> Result<(), RenameError> {
    let path = context_dir.join(JOURNAL_PATH);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(RenameError::Io(err)),
    }
    match path.parent() {
        Some(dir) => sync_dir(dir).map_err(RenameError::Io),
        None => Ok(()),
    }
}

/// Restores the files from a journal that a crash left (`--recover`), then removes the journal and rebuilds the graph.
///
/// It checks the journal first: the checksum, and each path. A bad journal
/// is an error and nothing changes. Then, for each file: bytes equal to the
/// old bytes need nothing, bytes equal to the planned new bytes are
/// restored, and any other bytes are listed in `conflicts` and left alone.
/// With a conflict, the journal is kept. It is safe to run again, also after
/// a crash during recover. It returns `found: false` when no journal exists.
pub fn recover(root: &Path, context_dir: &Path) -> Result<RecoverReport, RenameError> {
    let path = context_dir.join(JOURNAL_PATH);
    if fs::symlink_metadata(&path).is_err() {
        return Ok(RecoverReport {
            found: false,
            restored: Vec::new(),
            conflicts: Vec::new(),
        });
    }
    let lock = acquire_refresh_lock(context_dir)?.ok_or(RenameError::LockBusy)?;
    let result = recover_locked(root, context_dir, &path);
    drop(lock);
    result
}

/// Removes the journal without restoring anything. Call it after `recover`
/// reported conflicts and the user checked those files.
pub fn discard_journal(root: &Path, context_dir: &Path) -> Result<(), RenameError> {
    let path = context_dir.join(JOURNAL_PATH);
    if fs::symlink_metadata(&path).is_err() {
        return Ok(());
    }
    let lock = acquire_refresh_lock(context_dir)?.ok_or(RenameError::LockBusy)?;
    let removed = remove_journal(context_dir);
    drop(lock);
    removed?;
    rebuild_graph_only(root, context_dir)?;
    Ok(())
}

/// True when the journal path may be restored: a plain relative path, not
/// under `.git` or the context dir, and with no symlink on it. A missing
/// file is allowed here. The hash check lists it as a conflict.
fn safe_journal_path(root: &Path, context_dir: &Path, rel: &str) -> bool {
    let path = Path::new(rel);
    let plain = path.components().all(|c| matches!(c, Component::Normal(_)));
    if rel.is_empty() || !plain || path.is_absolute() {
        return false;
    }
    if path.components().any(|c| c.as_os_str() == ".git") {
        return false;
    }
    if root.join(path).starts_with(context_dir) {
        return false;
    }
    if let Ok(ctx_rel) = context_dir.strip_prefix(root) {
        if path.starts_with(ctx_rel) {
            return false;
        }
    }
    let mut cur = root.to_path_buf();
    for part in rel.split('/') {
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}

fn recover_locked(
    root: &Path,
    context_dir: &Path,
    path: &Path,
) -> Result<RecoverReport, RenameError> {
    let refuse = |text: String| Err(RenameError::Apply(format!("recover refused: {text}")));
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return refuse("the journal is a symlink".to_string());
    }
    let body = fs::read(path)?;
    let Ok(journal) = serde_json::from_slice::<Journal>(&body) else {
        return refuse("the journal does not parse".to_string());
    };
    if journal.version != 1 {
        return refuse("the journal has an unknown version".to_string());
    }
    if journal.checksum != journal.compute_checksum() {
        return refuse("the journal checksum does not match".to_string());
    }
    // Check the whole journal before the first write.
    let mut todo: Vec<(String, Vec<u8>)> = Vec::new();
    let mut conflicts: Vec<String> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    for file in &journal.files {
        if !safe_journal_path(root, context_dir, &file.path) {
            bad.push(format!("{} (unsafe path)", file.path));
            continue;
        }
        let Some(old) = hex_decode(&file.bytes_hex).filter(|b| hex_sha256(b) == file.sha256) else {
            bad.push(format!("{} (damaged journal bytes)", file.path));
            continue;
        };
        match fs::read(root.join(&file.path)) {
            Ok(now) => {
                let hash = hex_sha256(&now);
                if hash == file.sha256 {
                    // Already the old bytes: nothing to do.
                } else if hash == file.new_sha256 {
                    todo.push((file.path.clone(), old));
                } else {
                    conflicts.push(file.path.clone());
                }
            }
            Err(_) => conflicts.push(file.path.clone()),
        }
    }
    if !bad.is_empty() {
        return refuse(bad.join(", "));
    }
    let mut restored = Vec::new();
    for (rel, old) in todo {
        let full = root.join(&rel);
        let attrs = fs::metadata(&full).ok().map(|m| attrs_of(&m));
        write_atomic(&full, &old, attrs.as_ref())?;
        restored.push(rel);
    }
    if conflicts.is_empty() {
        remove_journal(context_dir)?;
        rebuild_graph_only(root, context_dir)?;
    } else {
        let _ = rebuild_graph_only(root, context_dir);
    }
    Ok(RecoverReport {
        found: true,
        restored,
        conflicts,
    })
}

// ---- diff ----

/// Prints the unified diff of the plan (the plan format). Each file shows
/// `--- a/path`, `+++ b/path` and one hunk per run of edited lines, with
/// `context` lines around each edit. It reads the files from `root`, so call
/// it before the write. It never panics: a file that no longer matches the
/// plan gets a one-line note and no hunks.
pub fn render_diff(plan: &Plan, root: &Path, context: usize) -> String {
    let mut out = String::new();
    for file in &plan.files {
        let bytes = match fs::read(root.join(&file.path)) {
            Ok(bytes) => bytes,
            Err(err) => {
                out.push_str(&format!("# skipped {}: {err}\n", file.path));
                continue;
            }
        };
        let matches_plan = hex_sha256(&bytes) == file.sha256
            && file
                .edits
                .iter()
                .all(|e| bytes.get(e.start..e.end) == Some(plan.old.as_bytes()));
        if !matches_plan {
            out.push_str(&format!(
                "# skipped {}: the file changed since the plan\n",
                file.path
            ));
            continue;
        }
        out.push_str(&format!("--- a/{0}\n+++ b/{0}\n", file.path));
        render_file(&mut out, &bytes, &file.edits, plan, context);
    }
    out
}

const NO_NEWLINE: &str = "\\ No newline at end of file\n";

fn render_file(out: &mut String, bytes: &[u8], edits: &[Edit], plan: &Plan, context: usize) {
    let mut starts = vec![0usize];
    starts.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(i, _)| i + 1),
    );
    let ends_nl = bytes.last() == Some(&b'\n');
    let count = if ends_nl {
        starts.len() - 1
    } else {
        starts.len()
    };
    let line_end = |i: usize| starts.get(i + 1).copied().unwrap_or(bytes.len());
    let shown = |text: &[u8]| {
        let text = text.strip_suffix(b"\n").unwrap_or(text);
        String::from_utf8_lossy(text).into_owned()
    };
    // The edits per line, as offsets inside the line. An identifier holds no
    // newline, so an edit never crosses a line.
    let mut by_line: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
    for edit in edits {
        let line = starts
            .partition_point(|&s| s <= edit.start)
            .saturating_sub(1);
        let from = edit.start.saturating_sub(starts[line]);
        let to = edit.end.saturating_sub(starts[line]);
        by_line.entry(line).or_default().push((from, to));
    }
    // One range per edited line, widened by the context, then merged.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &line in by_line.keys() {
        let from = line.saturating_sub(context);
        let to = line.saturating_add(context).min(count.saturating_sub(1));
        match ranges.last_mut() {
            Some(last) if from <= last.1 + 1 => last.1 = last.1.max(to),
            _ => ranges.push((from, to)),
        }
    }
    for (from, to) in ranges {
        let len = to - from + 1;
        let span = if len == 1 {
            format!("{}", from + 1)
        } else {
            format!("{},{len}", from + 1)
        };
        out.push_str(&format!("@@ -{span} +{span} @@\n"));
        let mut minus = String::new();
        let mut plus = String::new();
        for line in from..=to {
            let raw = &bytes[starts[line]..line_end(line)];
            let no_nl = !ends_nl && line + 1 == count;
            if let Some(spans) = by_line.get(&line) {
                let mut new = raw.to_vec();
                for &(a, b) in spans.iter().rev() {
                    if a <= b && b <= new.len() {
                        new.splice(a..b, plan.new.bytes());
                    }
                }
                minus.push_str(&format!("-{}\n", shown(raw)));
                plus.push_str(&format!("+{}\n", shown(&new)));
                if no_nl {
                    minus.push_str(NO_NEWLINE);
                    plus.push_str(NO_NEWLINE);
                }
            } else {
                out.push_str(&minus);
                out.push_str(&plus);
                minus.clear();
                plus.clear();
                out.push_str(&format!(" {}\n", shown(raw)));
                if no_nl {
                    out.push_str(NO_NEWLINE);
                }
            }
        }
        out.push_str(&minus);
        out.push_str(&plus);
    }
}
