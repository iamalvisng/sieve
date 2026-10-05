//! The repo file walk. Sieve builds one file list that feeds every later
//! extraction step.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory names Sieve never walks into, in any mode.
pub const SKIP_DIRS: [&str; 10] = [
    "node_modules",
    "dist",
    "build",
    "_build",
    "out",
    "target",
    "vendor",
    "coverage",
    "__pycache__",
    "venv",
];

/// The largest file size, in bytes, Sieve reads. A bigger file is skipped.
pub const MAX_FILE_BYTES: u64 = 1_000_000;

/// Lists every file Sieve should index under `root`.
///
/// Inside a git work tree, this runs `git ls-files` and keeps tracked and
/// untracked-but-not-ignored files. Outside a git work tree, this walks
/// the file system by hand. Both modes drop `SKIP_DIRS` and dot
/// components, and drop a file over `MAX_FILE_BYTES`. The result holds
/// repo-relative, forward-slash paths in this order:
/// the `git ls-files` order, or the sorted depth-first walk order with no
/// git. Only the follow path sorts, by UTF-16 code unit, and dedups.
pub fn walk_repo(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let includes = read_include_dirs(root);
    let follow_submodules = read_config_flag(root, "followSubmodules");
    let follow_nested = read_config_flag(root, "followNestedRepos");
    let follow = follow_submodules || follow_nested;
    let mut files = if !is_git_work_tree(root) {
        let mut out = Vec::new();
        walk_fs(root, Path::new(""), &includes, &mut out)?;
        out
    } else if follow {
        let mut walk = Follow {
            top: root.to_path_buf(),
            includes: &includes,
            submodules: follow_submodules,
            nested: follow_nested,
            active: Vec::new(),
        };
        walk.git_files(root)?
    } else {
        git_ls_files(root)?
    };

    files.retain(|rel| !has_skipped_component(rel, &includes) && under_size_cap(root, rel));
    if follow {
        // The follow path ends in a sort and dedup. The
        // shallow git path and the fs walk neither sort nor dedup.
        files.sort_by_cached_key(|a| to_slash(a).encode_utf16().collect::<Vec<u16>>());
        files.dedup();
    }
    Ok(files)
}

fn is_git_work_tree(root: &Path) -> bool {
    Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(root)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_ls_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root)
        .output()?;

    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| PathBuf::from(String::from_utf8_lossy(chunk).into_owned()))
        .collect())
}

/// The boundary-crossing walk: `git ls-files -t --stage` tells a gitlink (mode
/// 160000) and an untracked nested clone (a trailing-slash `?` record) from a
/// file. A followed child with its own `.git` is walked in turn, and its paths
/// take the child's prefix.
struct Follow<'a> {
    top: PathBuf,
    includes: &'a [String],
    submodules: bool,
    nested: bool,
    active: Vec<PathBuf>,
}

impl Follow<'_> {
    /// Files under `dir`, relative to `self.top`.
    fn git_files(&mut self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        if self.active.iter().any(|d| d == dir) {
            return Ok(Vec::new());
        }
        let prefix = dir
            .strip_prefix(&self.top)
            .unwrap_or(Path::new(""))
            .to_path_buf();
        let output = Command::new("git")
            .args([
                "ls-files",
                "-t",
                "--stage",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
            ])
            .current_dir(dir)
            .output();
        let Some(output) = output.ok().filter(|o| o.status.success()) else {
            // Sieve falls back to the filesystem for a broken child only.
            let mut out = Vec::new();
            walk_fs(dir, Path::new(""), self.includes, &mut out)?;
            return Ok(out.into_iter().map(|p| prefix.join(p)).collect());
        };
        // A path with several stage records (a merge) collapses to one entry.
        let mut entries: Vec<(String, bool, bool)> = Vec::new();
        for record in output.stdout.split(|b| *b == 0) {
            let record = String::from_utf8_lossy(record);
            let (Some(tag), Some(space), Some(body)) =
                (record.get(..1), record.get(1..2), record.get(2..))
            else {
                continue;
            };
            if space != " " {
                continue;
            }
            let (rel, gitlink, nested) = if tag == "?" {
                match body.strip_suffix('/') {
                    Some(rel) => (rel, false, true),
                    None => (body, false, false),
                }
            } else {
                let Some((meta, rel)) = body.split_once('\t') else {
                    continue;
                };
                (rel, meta.starts_with("160000 "), false)
            };
            if rel.is_empty() {
                continue;
            }
            match entries.iter_mut().find(|e| e.0 == rel) {
                Some(e) => {
                    e.1 |= gitlink;
                    e.2 |= nested;
                }
                None => entries.push((rel.to_string(), gitlink, nested)),
            }
        }
        self.active.push(dir.to_path_buf());
        let mut out = Vec::new();
        for (rel, gitlink, nested) in entries {
            let abs = dir.join(&rel);
            let from_top = prefix.join(&rel);
            if has_skipped_component(&from_top, self.includes) {
                continue;
            }
            if (gitlink && self.submodules) || (nested && self.nested) {
                if abs.join(".git").exists() {
                    out.extend(self.git_files(&abs)?);
                }
            } else {
                out.push(from_top);
            }
        }
        self.active.pop();
        Ok(out)
    }
}

/// Reads one persisted boolean of `<root>/.<product>/config.json`.
/// Only `true` turns it on.
fn read_config_flag(root: &Path, key: &str) -> bool {
    let path = root
        .join(crate::product().home_dir_name())
        .join("config.json");
    std::fs::read(path)
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
        .is_some_and(|v| v[key] == true)
}

/// Reads the persisted `build --include-dir` names for `root` from
/// `<root>/.<product>/config.json`. A name in the list leaves `SKIP_DIRS` for
/// this repo's walks. A missing or unreadable file gives an empty list.
pub fn read_include_dirs(root: &Path) -> Vec<String> {
    let path = root
        .join(crate::product().home_dir_name())
        .join("config.json");
    let Ok(body) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Vec::new();
    };
    value["includeDirs"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn walk_fs(
    root: &Path,
    rel: &Path,
    includes: &[String],
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let dir = if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    // The filesystem walk lets the read throw, so an unreadable dir fails the
    // whole walk with Node's `scandir` text (P2-01).
    let entries = std::fs::read_dir(&dir)
        .map_err(|err| crate::node_error::node_io_error(err, "scandir", &dir))?;

    // libuv's `scandir` sorts the names bytewise, so `readdirSync` does too.
    let mut entries = entries.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let child_rel = rel.join(&name);
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if metadata.is_dir() {
            if is_skipped_name(&name, includes) {
                continue;
            }
            walk_fs(root, &child_rel, includes, out)?;
        } else if metadata.is_file() {
            out.push(child_rel);
        }
    }
    Ok(())
}

/// The skip rule for a dir name: a dot name is always
/// skipped, a name in `includes` is never skipped, and a `SKIP_DIRS` name
/// is skipped otherwise.
pub(crate) fn is_skipped_name(name: &OsStr, includes: &[String]) -> bool {
    let name = name.to_string_lossy();
    if name.starts_with('.') {
        return true;
    }
    if includes.iter().any(|n| *n == name) {
        return false;
    }
    SKIP_DIRS.contains(&name.as_ref())
}

/// Reports whether any path component, the file name included, starts
/// with a dot or matches `SKIP_DIRS`. A dot-file is dropped, not kept, the
/// same as a dot-directory.
fn has_skipped_component(rel: &Path, includes: &[String]) -> bool {
    rel.components()
        .any(|component| is_skipped_name(component.as_os_str(), includes))
}

fn under_size_cap(root: &Path, rel: &Path) -> bool {
    match std::fs::symlink_metadata(root.join(rel)) {
        Ok(metadata) => metadata.is_file() && metadata.len() <= MAX_FILE_BYTES,
        Err(_) => false,
    }
}

fn to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("sieve-walk-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, vec![b'x'; bytes]).expect("write file");
    }

    fn as_slash_strings(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| to_slash(p)).collect()
    }

    #[test]
    fn walk_repo_plain_tree_skips_dirs_and_dotfiles() {
        let dir = TempDir::new("plain");
        write_file(&dir.path.join("a.ts"), 10);
        write_file(&dir.path.join("dist/x.js"), 10);
        write_file(&dir.path.join(".hidden/y.ts"), 10);
        write_file(&dir.path.join("node_modules/m/i.js"), 10);
        write_file(&dir.path.join("sub/b.py"), 10);

        let files = walk_repo(&dir.path).expect("walk_repo should not error");
        assert_eq!(as_slash_strings(&files), vec!["a.ts", "sub/b.py"]);
    }

    /// P1-70: a name in `<root>/.<product>/config.json`'s `includeDirs`
    /// leaves `SKIP_DIRS` for this repo; a dot dir never does.
    #[test]
    fn test_p1_70_walk_repo_include_dirs_lift_a_skip_dir_name() {
        let dir = TempDir::new("include");
        write_file(&dir.path.join("a.ts"), 10);
        write_file(&dir.path.join("dist/x.js"), 10);
        write_file(&dir.path.join("build/y.js"), 10);
        write_file(&dir.path.join(".hidden/z.ts"), 10);
        let config = dir.path.join(crate::product().home_dir_name());
        fs::create_dir_all(&config).expect("create config dir");
        fs::write(
            config.join("config.json"),
            r#"{"includeDirs":["dist",".hidden"]}"#,
        )
        .expect("write config");

        let files = walk_repo(&dir.path).expect("walk_repo should not error");
        assert_eq!(as_slash_strings(&files), vec!["a.ts", "dist/x.js"]);
    }

    /// P2-01: with no git, an unreadable dir fails the walk with Node's
    /// `scandir` text. The mode is restored before the temp dir drops.
    #[cfg(unix)]
    #[test]
    fn test_p2_01_walk_repo_fails_on_an_unreadable_dir_with_no_git() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("locked");
        write_file(&dir.path.join("a.ts"), 10);
        write_file(&dir.path.join("locked/b.ts"), 10);
        let locked = dir.path.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        let result = walk_repo(&dir.path);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod 755");
        let err = result.expect_err("an unreadable dir fails the walk");
        assert_eq!(
            err.to_string(),
            format!("EACCES: permission denied, scandir '{}'", locked.display())
        );
    }

    #[test]
    fn walk_repo_drops_files_over_the_size_cap() {
        let dir = TempDir::new("size");
        write_file(&dir.path.join("big.ts"), 1_000_001);
        write_file(&dir.path.join("ok.ts"), 1_000_000);

        let files = walk_repo(&dir.path).expect("walk_repo should not error");
        assert_eq!(as_slash_strings(&files), vec!["ok.ts"]);
    }

    #[test]
    fn walk_repo_git_mode_keeps_gitignore_filter_off_tracked_files() {
        let dir = TempDir::new("git");
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("git not available, skipping");
            return;
        }

        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir.path)
            .status()
            .expect("git init");
        assert!(init.success());

        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&dir.path)
            .status()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&dir.path)
            .status()
            .expect("git config name");

        write_file(&dir.path.join("dist/x.js"), 10);
        fs::write(dir.path.join(".gitignore"), "dist/\n").expect("write gitignore");

        Command::new("git")
            .args(["add", "-f", "dist/x.js", ".gitignore"])
            .current_dir(&dir.path)
            .status()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&dir.path)
            .status()
            .expect("git commit");

        let files = walk_repo(&dir.path).expect("walk_repo should not error");
        assert!(as_slash_strings(&files).is_empty());
    }

    #[test]
    fn walk_repo_p2_02_drops_a_dot_file_in_both_modes() {
        let dir = TempDir::new("dotfile");
        write_file(&dir.path.join("a.ts"), 10);
        write_file(&dir.path.join("src/.internal.ts"), 10);
        write_file(&dir.path.join(".eslintrc.js"), 10);

        let files = walk_repo(&dir.path).expect("walk_repo should not error");
        assert_eq!(as_slash_strings(&files), vec!["a.ts"]);

        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("git not available, skipping the git-mode half");
            return;
        }

        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir.path)
            .status()
            .expect("git init");
        assert!(init.success());

        Command::new("git")
            .args(["add", "-A"])
            .current_dir(&dir.path)
            .status()
            .expect("git add");

        let files = walk_repo(&dir.path).expect("walk_repo should not error");
        assert_eq!(as_slash_strings(&files), vec!["a.ts"]);
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::process::Stdio;

    /// Runs the system VCS tool in `dir` with the fixture identity. A missing
    /// tool fails the test, because a skipped order check proves nothing.
    fn git(dir: &Path, args: &[&str], stdin: &str) -> bool {
        let Ok(mut child) = Command::new("git")
            .args(["-c", "user.name=sieve-fixture"])
            .args(["-c", "user.email=fixture@sieve.local"])
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
        else {
            panic!("git is required to run the walk order tests");
        };
        let _ = child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes());
        child.wait().expect("git exits").success()
    }

    fn put(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, "x\n").expect("write");
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sieve-order-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn slashed(root: &Path) -> Vec<String> {
        let files = walk_repo(root).expect("walk");
        files.iter().map(|p| to_slash(p)).collect()
    }

    /// P2-31: tracked `m.js`, `a/x.py`, `zz.ts` and untracked `M.ts`,
    /// the walk keeps the `git ls-files` order, so the parse
    /// order is the list below, and `a/x.md` reads `# a/x.py`.
    #[test]
    fn test_p2_31_walk_repo_keeps_git_ls_files_order_for_untracked_files() {
        let dir = scratch("mixed");
        assert!(git(&dir, &["init", "-q"], ""));
        for rel in ["m.js", "a/x.py", "zz.ts"] {
            put(&dir, rel);
        }
        assert!(git(&dir, &["add", "."], ""));
        assert!(git(&dir, &["commit", "-qm", "init"], ""));
        put(&dir, "M.ts");
        put(&dir, "a/x.ts");
        assert_eq!(slashed(&dir), ["M.ts", "a/x.ts", "a/x.py", "m.js", "zz.ts"]);
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// P2-01: with no git, the walk is depth-first in sorted `readdirSync`
    /// order, so comes before `a-b.ts` (a parse order over
    /// 13 files with multibyte names). The names are many and mixed, so a
    /// raw file system order does not pass by chance.
    #[test]
    fn test_p2_01_walk_repo_without_git_is_depth_first_not_a_byte_sort() {
        let dir = scratch("plain");
        let names = [
            "b.ts", "a-b.ts", "a/x.ts", "a.ts", "é.ts", "z/é.ts", "Z.ts", "a0.ts", "a/B.ts",
            "Q/m.ts", "z/a.ts", "_u.ts", "1.ts",
        ];
        for rel in names {
            put(&dir, rel);
        }
        assert_eq!(
            slashed(&dir),
            [
                "1.ts", "Q/m.ts", "Z.ts", "_u.ts", "a/B.ts", "a/x.ts", "a-b.ts", "a.ts", "a0.ts",
                "b.ts", "z/a.ts", "z/é.ts", "é.ts"
            ]
        );
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// P2-31: a merge conflict lists `c.ts` three times in
    /// `git ls-files --cached`. The shallow path does not dedup, and
    /// the build reports `parsed: 4 of 4 files` for `a.ts` and `c.ts`.
    #[test]
    fn test_p2_31_walk_repo_keeps_a_merge_conflict_path_three_times() {
        let dir = scratch("merge");
        assert!(git(&dir, &["init", "-q"], ""));
        put(&dir, "a.ts");
        put(&dir, "c.ts");
        let blob = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let stages = (1..=3)
            .map(|n| format!("100644 {blob} {n}\tc.ts\n"))
            .collect::<String>();
        assert!(git(&dir, &["add", "a.ts"], ""));
        assert!(git(&dir, &["update-index", "--index-info"], &stages));
        assert_eq!(slashed(&dir), ["a.ts", "c.ts", "c.ts", "c.ts"]);
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// P2-31: the follow path ends in a sort and dedup, so
    /// it sorts. A run with `followNestedRepos` on the
    /// mixed repo gave this sorted list.
    #[test]
    fn test_p2_31_walk_repo_follow_path_sorts() {
        let dir = scratch("follow");
        assert!(git(&dir, &["init", "-q"], ""));
        for rel in ["m.js", "a/x.py", "zz.ts"] {
            put(&dir, rel);
        }
        assert!(git(&dir, &["add", "."], ""));
        assert!(git(&dir, &["commit", "-qm", "init"], ""));
        put(&dir, "M.ts");
        put(&dir, "a/x.ts");
        let home = dir.join(crate::product().home_dir_name());
        fs::create_dir_all(&home).expect("mkdir");
        fs::write(home.join("config.json"), r#"{"followNestedRepos":true}"#).expect("config");
        assert_eq!(slashed(&dir), ["M.ts", "a/x.py", "a/x.ts", "m.js", "zz.ts"]);
        fs::remove_dir_all(&dir).expect("cleanup");
    }
}
