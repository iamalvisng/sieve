//! Unit tests for the rename write half: apply, journal, recover, diff.

use std::fs;

use super::apply::{apply_inner, no_old_sites, precheck, write_raw_journal, Hooks};
use super::tests::{Repo, DEF, USE};
use super::*;

const DEF_G: &str = "export function g(x: number): number {\n  return x + 1;\n}\n";
const USE_G: &str = "import { g } from \"./def\";\nexport const y = g(1);\n";

fn two_files() -> Repo {
    Repo::new(&[("def.ts", DEF), ("use.ts", USE)])
}

fn read(repo: &Repo, path: &str) -> Vec<u8> {
    fs::read(repo.root().join(path)).expect("read file")
}

fn journal_path(repo: &Repo) -> std::path::PathBuf {
    repo.ctx().join(JOURNAL_PATH)
}

fn apply(repo: &Repo, expect: Option<&str>) -> ApplyReport {
    apply_plan(&repo.root(), &repo.ctx(), "f", "g", None, expect).expect("apply")
}

fn apply_hooked(repo: &Repo, hooks: Hooks) -> Result<ApplyReport, RenameError> {
    apply_inner(&repo.root(), &repo.ctx(), "f", "g", None, None, hooks)
}

fn assert_refused_journal(plan: &Plan) {
    assert_eq!(plan.status, Status::Refused);
    assert!(plan
        .refused
        .iter()
        .any(|r| r.reason == Reason::JournalPresent));
}

/// Stops the apply before the file at `index` is written, as a crash would.
fn crash_before(repo: &Repo, index: usize) {
    let hooks = Hooks {
        crash_before: Some(index),
        ..Hooks::default()
    };
    assert!(apply_hooked(repo, hooks).is_err());
}

fn crash_after_first_file(repo: &Repo) {
    crash_before(repo, 1);
    assert_eq!(read(repo, "def.ts"), DEF_G.as_bytes());
    assert_eq!(read(repo, "use.ts"), USE.as_bytes());
}

// ---- apply ----

#[test]
fn test_rn3a_write_applies_every_edit() {
    let repo = two_files();
    let plan = repo.plan("f", "g");
    let out = apply(&repo, Some(&plan.plan_id));
    assert_eq!(out.status, ApplyStatus::Written, "{:?}", out.message);
    assert_eq!(out.files, vec!["def.ts", "use.ts"]);
    assert_eq!(read(&repo, "def.ts"), DEF_G.as_bytes());
    assert_eq!(read(&repo, "use.ts"), USE_G.as_bytes());
    assert!(!journal_path(&repo).exists());
    // No temp file is left behind.
    let left = fs::read_dir(repo.root())
        .expect("list")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".sieve-tmp"))
        .count();
    assert_eq!(left, 0);
}

#[test]
fn test_rn3a_expect_mismatch_changes_no_file() {
    let repo = two_files();
    let out = apply(&repo, Some("00000000"));
    assert_eq!(out.status, ApplyStatus::ExpectMismatch);
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    assert_eq!(read(&repo, "use.ts"), USE.as_bytes());
    assert!(!journal_path(&repo).exists());
}

#[test]
fn test_rn3a_changed_file_gives_new_plan_id() {
    let repo = two_files();
    let before = repo.plan("f", "g");
    repo.write("use.ts", format!("// note\n{USE}").as_bytes());
    let after = repo.plan("f", "g");
    assert_ne!(before.plan_id, after.plan_id);
    let out = apply(&repo, Some(&before.plan_id));
    assert_eq!(out.status, ApplyStatus::ExpectMismatch);
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
}

#[test]
fn test_rn3a_write_refreshes_the_graph() {
    let repo = two_files();
    assert_eq!(apply(&repo, None).status, ApplyStatus::Written);
    let new = plan_rename(&repo.root(), &repo.ctx(), "g", "h", None).expect("plan g");
    assert_eq!(new.target.id, "def.ts#g");
    let old = plan_rename(&repo.root(), &repo.ctx(), "f", "h", None);
    assert!(matches!(old, Err(RenameError::Symbol(_))));
}

#[test]
fn test_rn3a_forced_post_check_failure_rolls_back() {
    let repo = two_files();
    let hooks = Hooks {
        fail_post_check: true,
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(out.status, ApplyStatus::RolledBack);
    assert_eq!(out.files, vec!["def.ts", "use.ts"]);
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    assert_eq!(read(&repo, "use.ts"), USE.as_bytes());
    // The design removes the journal after a complete rollback.
    assert!(!journal_path(&repo).exists());
    // The graph is rebuilt: the old name resolves again.
    assert_eq!(repo.plan("f", "g").status, Status::Ready);
}

#[test]
fn test_rn3a_write_failure_restores_written_files() {
    let repo = two_files();
    let hooks = Hooks {
        fail_write_at: Some(1),
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(out.status, ApplyStatus::RolledBack);
    assert!(out.message.is_some_and(|m| m.contains("write")));
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    assert_eq!(read(&repo, "use.ts"), USE.as_bytes());
    assert!(!journal_path(&repo).exists());
}

#[test]
fn test_rn3a_restore_failure_keeps_the_journal() {
    let repo = two_files();
    let hooks = Hooks {
        fail_write_at: Some(1),
        fail_restore: true,
        ..Hooks::default()
    };
    match apply_hooked(&repo, hooks) {
        Err(RenameError::RestoreFailed {
            why,
            files,
            journal,
        }) => {
            assert!(why.contains("write"), "{why}");
            assert!(files.contains(&"def.ts".to_string()));
            assert_eq!(journal, journal_path(&repo));
        }
        other => panic!("want RestoreFailed, got {other:?}"),
    }
    assert!(journal_path(&repo).exists());
}

#[test]
fn test_rn3a_journal_exists_before_the_first_write() {
    let repo = two_files();
    crash_before(&repo, 0);
    assert!(journal_path(&repo).exists());
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    let text = fs::read_to_string(journal_path(&repo)).expect("journal");
    assert!(text.contains("def.ts") && text.contains("use.ts"));
    assert!(text.contains("checksum"));
}

// ---- post-check ----

#[test]
fn test_rn3a_post_check_c_lost_link_rolls_back() {
    let user = "import { f } from \"./def\";\nexport function h() {\n  return f(1);\n}\n";
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", user)]);
    // The scan "skips" the importer, so the graph loses the h -> f link.
    let hooks = Hooks {
        drop_file: Some("use.ts"),
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(out.status, ApplyStatus::RolledBack, "{:?}", out.message);
    assert!(out.message.is_some_and(|m| m.contains("is lost")));
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    assert_eq!(read(&repo, "use.ts"), user.as_bytes());
    assert!(!journal_path(&repo).exists());
}

#[test]
fn test_rn3a_post_check_a_changed_file_is_not_overwritten() {
    let repo = two_files();
    let hooks = Hooks {
        tamper_file: Some("use.ts"),
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(out.status, ApplyStatus::RollbackConflict);
    assert_eq!(out.conflicts, vec!["use.ts"]);
    assert_eq!(out.files, vec!["def.ts"]);
    // The other file is restored. The changed file keeps the later change.
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    let tampered = format!("{USE_G}\n// tampered\n");
    assert_eq!(read(&repo, "use.ts"), tampered.as_bytes());
    assert!(journal_path(&repo).exists());
    // recover lists the file, and discard_journal ends the state.
    let done = recover(&repo.root(), &repo.ctx()).expect("recover");
    assert_eq!(done.conflicts, vec!["use.ts"]);
    assert!(done.restored.is_empty());
    discard_journal(&repo.root(), &repo.ctx()).expect("discard");
    assert!(!journal_path(&repo).exists());
    assert_eq!(read(&repo, "use.ts"), tampered.as_bytes());
}

#[test]
fn test_rn3a_post_check_d_finds_a_left_old_name() {
    let repo = two_files();
    let root = repo.root();
    // Nothing was renamed here, so the old name is still in both files.
    assert!(no_old_sites(&root, &["def.ts", "use.ts"], "f").is_err());
    assert!(no_old_sites(&root, &["def.ts", "use.ts"], "zzz").is_ok());
}

// ---- recover ----

#[test]
fn test_rn3a_recover_restores_a_half_written_rename() {
    let repo = two_files();
    crash_after_first_file(&repo);
    // Planning and applying refuse while the journal exists.
    assert_refused_journal(&repo.plan("g", "h"));
    let out = apply_plan(&repo.root(), &repo.ctx(), "g", "h", None, None).expect("apply");
    assert_eq!(out.status, ApplyStatus::Refused);
    assert_refused_journal(&out.plan);
    assert_eq!(read(&repo, "def.ts"), DEF_G.as_bytes());

    let done = recover(&repo.root(), &repo.ctx()).expect("recover");
    assert!(done.found);
    assert_eq!(done.restored, vec!["def.ts"]);
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    assert_eq!(read(&repo, "use.ts"), USE.as_bytes());
    assert!(!journal_path(&repo).exists());
    assert_eq!(repo.plan("f", "g").status, Status::Ready);
}

#[test]
fn test_rn3a_recover_twice_is_safe() {
    let repo = two_files();
    crash_after_first_file(&repo);
    let first = recover(&repo.root(), &repo.ctx()).expect("recover");
    assert!(first.found);
    let second = recover(&repo.root(), &repo.ctx()).expect("recover again");
    assert!(!second.found);
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
}

#[test]
fn test_rn3a_recover_after_a_crash_before_any_write() {
    let repo = two_files();
    crash_before(&repo, 0);
    let done = recover(&repo.root(), &repo.ctx()).expect("recover");
    assert!(done.found);
    assert!(done.restored.is_empty());
    assert!(!journal_path(&repo).exists());
}

#[test]
fn test_rn3a_recover_without_journal_finds_nothing() {
    let repo = two_files();
    let done = recover(&repo.root(), &repo.ctx()).expect("recover");
    assert!(!done.found);
    assert!(done.restored.is_empty());
}

#[test]
fn test_rn3a_tampered_journal_is_refused() {
    let repo = two_files();
    crash_after_first_file(&repo);
    let text = fs::read_to_string(journal_path(&repo)).expect("journal");
    let edited = text.replace("\"new\":\"g\"", "\"new\":\"h\"");
    assert_ne!(text, edited);
    fs::write(journal_path(&repo), edited).expect("tamper");
    let out = recover(&repo.root(), &repo.ctx());
    assert!(matches!(out, Err(RenameError::Apply(ref t)) if t.contains("checksum")));
    assert_eq!(read(&repo, "def.ts"), DEF_G.as_bytes());
    assert!(journal_path(&repo).exists());
}

#[test]
fn test_rn3a_recover_refuses_unsafe_paths() {
    let repo = two_files();
    fs::create_dir_all(repo.root().join(".git")).expect("mkdir");
    fs::write(repo.root().join(".git/config"), "x").expect("write");
    for bad in ["../evil.ts", "/etc/evil.ts", ".git/config", "a/../def.ts"] {
        write_raw_journal(&repo.ctx(), &[(bad, b"x", b"y")]).expect("journal");
        let out = recover(&repo.root(), &repo.ctx());
        assert!(
            matches!(out, Err(RenameError::Apply(ref t)) if t.contains(bad)),
            "{bad}: {out:?}"
        );
        assert!(journal_path(&repo).exists());
    }
    assert!(!repo.dir.join("evil.ts").exists());
    assert_eq!(
        fs::read(repo.root().join(".git/config")).expect("read"),
        b"x"
    );
}

#[test]
fn test_rn3a_recover_refuses_a_path_under_the_context_dir() {
    let repo = two_files();
    let ctx = repo.root().join(".sieve");
    fs::create_dir_all(ctx.join("keep")).expect("mkdir");
    fs::write(ctx.join("keep/x.ts"), "x").expect("write");
    write_raw_journal(&ctx, &[(".sieve/keep/x.ts", b"x", b"y")]).expect("journal");
    let out = recover(&repo.root(), &ctx);
    assert!(matches!(out, Err(RenameError::Apply(_))), "{out:?}");
}

#[cfg(unix)]
#[test]
fn test_rn3a_recover_refuses_a_symlinked_path() {
    let repo = two_files();
    fs::rename(repo.root().join("use.ts"), repo.root().join("real.ts")).expect("move");
    std::os::unix::fs::symlink("real.ts", repo.root().join("use.ts")).expect("link");
    write_raw_journal(&repo.ctx(), &[("use.ts", USE.as_bytes(), USE_G.as_bytes())])
        .expect("journal");
    repo.write("real.ts", USE_G.as_bytes());
    let out = recover(&repo.root(), &repo.ctx());
    assert!(matches!(out, Err(RenameError::Apply(ref t)) if t.contains("use.ts")));
    assert_eq!(read(&repo, "real.ts"), USE_G.as_bytes());
}

#[test]
fn test_rn3a_recover_lists_a_file_changed_by_someone_else() {
    let repo = two_files();
    crash_after_first_file(&repo);
    repo.write("def.ts", b"// edited by hand\n");
    let done = recover(&repo.root(), &repo.ctx()).expect("recover");
    assert!(done.found);
    assert_eq!(done.conflicts, vec!["def.ts"]);
    // use.ts was never written, so it holds the old bytes: nothing to do.
    assert!(done.restored.is_empty());
    assert_eq!(read(&repo, "def.ts"), b"// edited by hand\n");
    assert!(journal_path(&repo).exists());
    // The same state on a second call.
    let again = recover(&repo.root(), &repo.ctx()).expect("recover again");
    assert_eq!(again.conflicts, vec!["def.ts"]);
    discard_journal(&repo.root(), &repo.ctx()).expect("discard");
    assert!(!journal_path(&repo).exists());
    assert!(!recover(&repo.root(), &repo.ctx()).expect("recover").found);
}

#[test]
fn test_rn3a_recover_restores_the_rest_when_one_file_conflicts() {
    let repo = two_files();
    // Both files hold the new bytes, then one is edited by hand.
    let hooks = Hooks {
        crash_before: Some(2),
        ..Hooks::default()
    };
    assert!(apply_hooked(&repo, hooks).is_err());
    assert_eq!(read(&repo, "use.ts"), USE_G.as_bytes());
    repo.write("def.ts", b"// edited by hand\n");
    let done = recover(&repo.root(), &repo.ctx()).expect("recover");
    assert_eq!(done.restored, vec!["use.ts"]);
    assert_eq!(done.conflicts, vec!["def.ts"]);
    assert_eq!(read(&repo, "use.ts"), USE.as_bytes());
    assert!(journal_path(&repo).exists());
}

#[test]
fn test_rn3a_discard_without_journal_is_fine() {
    let repo = two_files();
    discard_journal(&repo.root(), &repo.ctx()).expect("discard");
}

// ---- files ----

#[cfg(unix)]
#[test]
fn test_rn3a_file_mode_is_kept() {
    use std::os::unix::fs::PermissionsExt;
    let repo = two_files();
    let path = repo.root().join("use.ts");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o751)).expect("chmod");
    assert_eq!(apply(&repo, None).status, ApplyStatus::Written);
    let mode = fs::metadata(&path).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, 0o751);
}

#[cfg(unix)]
#[test]
fn test_rn3a_read_only_file_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let repo = two_files();
    let path = repo.root().join("use.ts");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).expect("chmod");
    let out = apply(&repo, None);
    assert_eq!(out.status, ApplyStatus::Refused);
    assert!(out
        .plan
        .refused
        .iter()
        .any(|r| r.path == "use.ts" && r.text.contains("read-only")));
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
}

#[test]
fn test_rn3a_crlf_and_bom_keep_their_bytes() {
    let def = "\u{feff}export function f(x: number): number {\r\n  return x + 1;\r\n}\r\n";
    let usr = "import { f } from \"./def\";\r\nexport const y = f(1);\r\n";
    let repo = Repo::new(&[("def.ts", def), ("use.ts", usr)]);
    let out = apply(&repo, None);
    assert_eq!(out.status, ApplyStatus::Written, "{:?}", out.plan.refused);
    assert_eq!(read(&repo, "def.ts"), def.replace("f(x", "g(x").as_bytes());
    assert_eq!(
        read(&repo, "use.ts"),
        usr.replace("{ f }", "{ g }")
            .replace("= f(", "= g(")
            .as_bytes()
    );
}

#[cfg(unix)]
#[test]
fn test_rn3a_symlinked_path_fails_the_precheck() {
    let repo = two_files();
    let plan = repo.plan("f", "g");
    assert_eq!(plan.status, Status::Ready);
    // A file turns into a symlink after planning.
    fs::rename(repo.root().join("use.ts"), repo.root().join("real.ts")).expect("move");
    std::os::unix::fs::symlink("real.ts", repo.root().join("use.ts")).expect("link");
    let err = precheck(&repo.root(), &plan).expect_err("refuses");
    assert!(err
        .iter()
        .any(|r| r.path == "use.ts" && r.reason == Reason::UnsupportedSite));
    assert_eq!(read(&repo, "real.ts"), USE.as_bytes());
}

#[test]
fn test_rn3a_stale_hash_fails_the_precheck() {
    let repo = two_files();
    let plan = repo.plan("f", "g");
    repo.write("use.ts", b"// other\n");
    let err = precheck(&repo.root(), &plan).expect_err("refuses");
    assert!(err.iter().any(|r| r.reason == Reason::StaleIndex));
}

// ---- diff ----

#[test]
fn test_rn3a_render_diff_one_file_context_0() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    let plan = repo.plan("f", "g");
    let want = "--- a/def.ts\n+++ b/def.ts\n@@ -1 +1 @@\n\
                -export function f(x: number): number {\n\
                +export function g(x: number): number {\n";
    assert_eq!(render_diff(&plan, &repo.root(), 0), want);
}

#[test]
fn test_rn3a_render_diff_two_files_context_0() {
    let repo = two_files();
    let plan = repo.plan("f", "g");
    let want = "--- a/def.ts\n+++ b/def.ts\n@@ -1 +1 @@\n\
                -export function f(x: number): number {\n\
                +export function g(x: number): number {\n\
                --- a/use.ts\n+++ b/use.ts\n@@ -1,2 +1,2 @@\n\
                -import { f } from \"./def\";\n\
                -export const y = f(1);\n\
                +import { g } from \"./def\";\n\
                +export const y = g(1);\n";
    assert_eq!(render_diff(&plan, &repo.root(), 0), want);
}

#[test]
fn test_rn3a_render_diff_context_2_and_separate_hunks() {
    let body = "import { f } from \"./def\";\n\na\nb\nc\nd\ne\nf(1);\n";
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", body)]);
    let plan = repo.plan("f", "g");
    let ctx2 = render_diff(&plan, &repo.root(), 2);
    let want_use = "--- a/use.ts\n+++ b/use.ts\n\
                    @@ -1,3 +1,3 @@\n\
                    -import { f } from \"./def\";\n\
                    +import { g } from \"./def\";\n \n a\n\
                    @@ -6,3 +6,3 @@\n d\n e\n-f(1);\n+g(1);\n";
    assert!(ctx2.ends_with(want_use), "{ctx2}");
    let want_def = "--- a/def.ts\n+++ b/def.ts\n@@ -1,3 +1,3 @@\n\
                    -export function f(x: number): number {\n\
                    +export function g(x: number): number {\n\
                    \x20\x20\x20return x + 1;\n }\n";
    assert!(ctx2.starts_with(want_def), "{ctx2}");
    // With context 0 the two edits of use.ts stay in two hunks.
    let ctx0 = render_diff(&plan, &repo.root(), 0);
    assert_eq!(ctx0.matches("@@ -").count(), 3);
}

#[test]
fn test_rn3a_render_diff_two_edits_on_one_line() {
    let body = "import { f } from \"./def\";\nexport const y = f(1) + f(2);\n";
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", body)]);
    let diff = render_diff(&repo.plan("f", "g"), &repo.root(), 0);
    let want = "--- a/use.ts\n+++ b/use.ts\n@@ -1,2 +1,2 @@\n\
                -import { f } from \"./def\";\n\
                -export const y = f(1) + f(2);\n\
                +import { g } from \"./def\";\n\
                +export const y = g(1) + g(2);\n";
    assert!(diff.ends_with(want), "{diff}");
}

#[test]
fn test_rn3a_render_diff_crlf_keeps_the_carriage_return() {
    let usr = "import { f } from \"./def\";\r\nexport const y = f(1);\r\n";
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", usr)]);
    let diff = render_diff(&repo.plan("f", "g"), &repo.root(), 0);
    // The shown lines keep the carriage return, so `patch` applies the diff.
    assert!(diff.contains("-export const y = f(1);\r\n"), "{diff:?}");
    assert!(diff.contains("+export const y = g(1);\r\n"), "{diff:?}");
}

#[test]
fn test_rn3a_render_diff_marks_a_missing_final_newline() {
    let usr = "import { f } from \"./def\";\nexport const y = f(1);";
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", usr)]);
    let diff = render_diff(&repo.plan("f", "g"), &repo.root(), 0);
    let want = "--- a/use.ts\n+++ b/use.ts\n@@ -1,2 +1,2 @@\n\
                -import { f } from \"./def\";\n\
                -export const y = f(1);\n\
                \\ No newline at end of file\n\
                +import { g } from \"./def\";\n\
                +export const y = g(1);\n\
                \\ No newline at end of file\n";
    assert!(diff.ends_with(want), "{diff}");
}

#[test]
fn test_rn3a_render_diff_skips_a_changed_file_without_panic() {
    let repo = two_files();
    let plan = repo.plan("f", "g");
    repo.write("use.ts", b"");
    let diff = render_diff(&plan, &repo.root(), 2);
    assert!(diff.contains("# skipped use.ts"), "{diff}");
    assert!(diff.contains("--- a/def.ts"));
    fs::remove_file(repo.root().join("def.ts")).expect("rm");
    assert!(render_diff(&plan, &repo.root(), 0).contains("# skipped def.ts"));
}

// ---- post-check (d) and (e), and a property call ----

#[test]
fn test_rn3a_post_check_e_missed_unused_import_rolls_back() {
    let third = "import { f } from \"./def\";\n";
    let files = [("def.ts", DEF), ("use.ts", USE), ("third.ts", third)];
    // Without the hook the plan sees the unused import and renames it.
    let ok = Repo::new(&files);
    assert_eq!(apply(&ok, None).status, ApplyStatus::Written);
    assert_eq!(
        read(&ok, "third.ts"),
        third.replace("{ f }", "{ g }").as_bytes()
    );
    // With the hook the plan misses it. No graph edge shows the miss, so
    // only the name count in check (e) can see it.
    let repo = Repo::new(&files);
    let hooks = Hooks {
        drop_file: Some("third.ts"),
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(out.status, ApplyStatus::RolledBack, "{:?}", out.message);
    let message = out.message.unwrap_or_default();
    assert!(message.contains("appears 1 time"), "{message}");
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    assert_eq!(read(&repo, "use.ts"), USE.as_bytes());
    assert_eq!(read(&repo, "third.ts"), third.as_bytes());
    assert!(!journal_path(&repo).exists());
}

#[test]
fn test_rn3a_post_check_d_left_old_name_rolls_back_through_apply() {
    // A local function with an export clause: two edits, no importer.
    let def = "function f(x: number): number {\n  return x + 1;\n}\nexport { f };\n";
    let repo = Repo::new(&[("def.ts", def)]);
    assert_eq!(repo.plan("f", "g").files[0].edits.len(), 2);
    // The plan loses the export clause edit, so `f` stays in a touched file.
    let hooks = Hooks {
        skip_edit: Some(("def.ts", 1)),
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(out.status, ApplyStatus::RolledBack, "{:?}", out.message);
    let message = out.message.unwrap_or_default();
    assert!(message.contains("remains"), "{message}");
    assert_eq!(read(&repo, "def.ts"), def.as_bytes());
}

#[test]
fn test_rn3a_property_call_in_the_defining_file_does_not_block_the_write() {
    // `cfg.f()` is a property call. The graph links `run` to `f` by name
    // only, and the rename rightly leaves `cfg.f()` alone.
    let def = "export function f(x: number): number {\n  return x + 1;\n}\n\
               export function run(cfg: { f(): number }): number {\n  return cfg.f();\n}\n";
    let repo = Repo::new(&[("def.ts", def), ("use.ts", USE)]);
    let out = apply(&repo, None);
    assert_eq!(out.status, ApplyStatus::Written, "{:?}", out.message);
    let want = def.replacen("function f(", "function g(", 1);
    assert_eq!(read(&repo, "def.ts"), want.as_bytes());
    assert_eq!(read(&repo, "use.ts"), USE_G.as_bytes());
}

// ---- post-check (c') and (f), sync failure, late conflict ----

#[test]
fn test_rn3a_post_check_c_prime_hidden_bare_call_rolls_back() {
    // A same-file bare call. The graph links `h` to `f`. The hook hides the
    // call edit from the plan, so `f(1)` is left in `h`.
    let def = "export function f(x: number): number {\n  return x + 1;\n}\n\
               export function h(): number {\n  return f(1);\n}\n";
    let repo = Repo::new(&[("def.ts", def)]);
    assert_eq!(repo.plan("f", "g").files[0].edits.len(), 2);
    let hooks = Hooks {
        skip_edit: Some(("def.ts", 1)),
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(out.status, ApplyStatus::RolledBack, "{:?}", out.message);
    let message = out.message.unwrap_or_default();
    assert!(message.contains("is lost"), "{message}");
    assert_eq!(read(&repo, "def.ts"), def.as_bytes());
}

#[test]
fn test_rn3a_post_check_f_over_edit_of_an_unrelated_symbol_rolls_back() {
    // `other.ts` has its own `f`. The hook adds it to the plan, as an
    // over-edit would. Its node id changes, which check (f) sees.
    let other = "export function f(): number {\n  return 2;\n}\n";
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", USE), ("other.ts", other)]);
    let hooks = Hooks {
        add_file: Some("other.ts"),
        ..Hooks::default()
    };
    let out = apply_inner(
        &repo.root(),
        &repo.ctx(),
        "def.ts#f",
        "g",
        None,
        None,
        hooks,
    )
    .expect("apply");
    assert_eq!(out.status, ApplyStatus::RolledBack, "{:?}", out.message);
    let message = out.message.unwrap_or_default();
    assert!(message.contains("other.ts#f is gone"), "{message}");
    assert_eq!(read(&repo, "other.ts"), other.as_bytes());
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    // Without the hook the same rename is written, and other.ts is intact.
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", USE), ("other.ts", other)]);
    let ok = apply_plan(&repo.root(), &repo.ctx(), "def.ts#f", "g", None, None).expect("apply");
    assert_eq!(ok.status, ApplyStatus::Written, "{:?}", ok.message);
    assert_eq!(read(&repo, "other.ts"), other.as_bytes());
}

#[test]
fn test_rn3a_sync_failure_after_a_passed_check_says_what_to_do() {
    let repo = two_files();
    let ctx = repo.root().join(".sieve");
    let hooks = Hooks {
        fail_sync: true,
        ..Hooks::default()
    };
    let out = apply_inner(&repo.root(), &ctx, "f", "g", None, None, hooks);
    let Err(RenameError::Apply(text)) = out else {
        panic!("want Apply, got {out:?}");
    };
    assert!(
        text.contains("complete on disk and passed its check"),
        "{text}"
    );
    assert!(text.contains("sieve rename --discard-journal"), "{text}");
    assert!(text.contains("sieve rename --recover"), "{text}");
    assert!(text.contains(".sieve/.cache/rename-journal.json"), "{text}");
    let root_text = repo.root().to_string_lossy().into_owned();
    assert!(!text.contains(&root_text), "{text}");
    assert!(ctx.join(JOURNAL_PATH).exists());
    assert_eq!(read(&repo, "def.ts"), DEF_G.as_bytes());
}

#[test]
fn test_rn3a_rollback_after_another_failure_leaves_a_changed_file_alone() {
    let repo = two_files();
    // The file changes inside the post-check, then another check fails.
    let hooks = Hooks {
        tamper_late: Some("use.ts"),
        fail_post_check: true,
        ..Hooks::default()
    };
    let out = apply_hooked(&repo, hooks).expect("apply");
    assert_eq!(
        out.status,
        ApplyStatus::RollbackConflict,
        "{:?}",
        out.message
    );
    assert_eq!(out.conflicts, vec!["use.ts"]);
    assert_eq!(out.files, vec!["def.ts"]);
    assert_eq!(read(&repo, "def.ts"), DEF.as_bytes());
    let late = format!("{USE_G}\n// late\n");
    assert_eq!(read(&repo, "use.ts"), late.as_bytes());
    assert!(journal_path(&repo).exists());
}
