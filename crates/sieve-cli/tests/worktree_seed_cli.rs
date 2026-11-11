//! A first query in a linked worktree builds its own index from the files of
//! the worktree, and answers like a full no-reuse build.

mod support;

use std::path::Path;
use std::process::Command;

use support::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn sieve(dir: &Path, args: &[&str]) -> String {
    let out = support::sieve_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run sieve");
    assert!(out.status.success(), "sieve {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn change(wt: &Path) {
    std::fs::write(wt.join("a.py"), "def alpha():\n    return gamma()\n").unwrap();
    std::fs::write(wt.join("c.py"), "def gamma():\n    return delta()\n").unwrap();
    std::fs::remove_file(wt.join("b.py")).unwrap();
    std::fs::rename(wt.join("d.py"), wt.join("e.py")).unwrap();
}

#[test]
fn a_first_query_in_a_worktree_finds_the_same_as_a_no_reuse_build() {
    let temp = TempDir::new("wt-seed");
    let base = std::fs::canonicalize(&temp.path).unwrap();
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    for (name, text) in [
        ("a.py", "def alpha():\n    return beta()\n"),
        ("b.py", "def beta():\n    return 1\n"),
        ("d.py", "def delta():\n    return alpha()\n"),
    ] {
        std::fs::write(main.join(name), text).unwrap();
    }
    git(&main, &["init", "-q"]);
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "x"]);
    sieve(&main, &["build"]);

    let seeded = main.join(".claude/worktrees/seeded");
    let cold = base.join("cold");
    for wt in [&seeded, &cold] {
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        change(wt);
        git(wt, &["add", "-A"]);
    }
    let first = sieve(&seeded, &["find", "gamma"]);
    sieve(&cold, &["build", "--no-reuse"]);
    let second = sieve(&cold, &["find", "gamma", "--no-refresh"]);
    assert!(first.contains("c.py"), "{first}");
    assert_eq!(first, second);
    // The nested worktree built its own index and left the main index alone.
    assert!(seeded.join("sieve/.graph/wiring.json").is_file());
    assert!(!sieve(&main, &["find", "gamma", "--no-refresh"]).contains("c.py"));
}
