//! Finds the main checkout of a linked `git worktree`.

use std::path::{Component, Path, PathBuf};

/// Resolves `.` and `..` segments with no filesystem read.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The main checkout of a linked `git worktree`, or `None` when `root`
/// is not one. The `.git` of a linked worktree is a file whose `gitdir:`
/// line points under `<main>/.git/worktrees/`. The `commondir` file in that
/// folder names `<main>/.git`. A bare main, a submodule and a worktree whose
/// admin folder was pruned all give `None`.
pub fn main_worktree_root(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if std::fs::metadata(&dot).ok()?.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let line = text.split('\n').find(|l| l.starts_with("gitdir:"))?;
    let target = line["gitdir:".len()..].trim();
    if target.is_empty() {
        return None;
    }
    let gitdir = normalize(&root.join(target));
    if gitdir.parent()?.file_name()? != "worktrees" {
        return None;
    }
    let rel = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    let common = normalize(&gitdir.join(rel));
    if common.file_name()? != ".git" || !common.is_dir() {
        return None;
    }
    let main = common.parent()?.to_path_buf();
    if normalize(&main) == normalize(root) {
        return None;
    }
    Some(main)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "x"]);
    }

    #[test]
    fn nested_and_sibling_worktrees_name_the_main_checkout() {
        let tmp = tempdir("wt-both");
        let main = std::fs::canonicalize(&tmp).unwrap().join("main");
        repo(&main);
        let nested = main.join(".claude/worktrees/x");
        let sibling = main.parent().unwrap().join("sib");
        git(&main, &["worktree", "add", "-q", nested.to_str().unwrap()]);
        git(&main, &["worktree", "add", "-q", sibling.to_str().unwrap()]);
        assert_eq!(main_worktree_root(&nested), Some(main.clone()));
        assert_eq!(main_worktree_root(&sibling), Some(main.clone()));
        assert_eq!(main_worktree_root(&main), None);
        // A pruned admin folder gives None.
        std::fs::remove_dir_all(main.join(".git/worktrees/sib")).unwrap();
        assert_eq!(main_worktree_root(&sibling), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn bare_main_and_submodule_give_none() {
        let tmp = tempdir("wt-none");
        let base = std::fs::canonicalize(&tmp).unwrap();
        let bare = base.join("bare.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);
        let seed = base.join("seed");
        repo(&seed);
        git(
            &seed,
            &["push", "-q", bare.to_str().unwrap(), "HEAD:refs/heads/main"],
        );
        let bare_wt = base.join("bare-wt");
        git(
            &bare,
            &["worktree", "add", "-q", bare_wt.to_str().unwrap(), "main"],
        );
        assert_eq!(main_worktree_root(&bare_wt), None);

        let parent = base.join("parent");
        repo(&parent);
        git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                seed.to_str().unwrap(),
                "sub",
            ],
        );
        assert_eq!(main_worktree_root(&parent.join("sub")), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sieve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
