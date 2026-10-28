//! The command names `query`, `find`, `impact` and `symbols`, and their
//! old names `ask`, `grep`, `blast` and `skeleton` as aliases.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("read file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

fn run(root: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .args(args)
        .current_dir(root)
        .output()
        .expect("run sieve")
}

/// Copies the `basic` fixture and builds its index.
fn built_fixture() -> TempDir {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("command-names");
    copy_dir(&fixture, &temp.path);
    let out = run(&temp.path, &["build", "."]);
    assert_eq!(out.status.code(), Some(0), "build failed");
    temp
}

/// The command lines to compare: the new name, the old name, the arguments.
const PAIRS: [(&str, &str, &[&str]); 4] = [
    ("query", "ask", &["main", "--def"]),
    ("find", "grep", &["main", "--fixed"]),
    ("impact", "blast", &["--format", "json"]),
    ("symbols", "skeleton", &["src/app.ts"]),
];

/// Each new name runs and exits 0 on the fixture.
#[test]
fn each_new_command_name_runs() {
    let temp = built_fixture();
    for (new, _, args) in PAIRS {
        let mut argv = vec![new];
        argv.extend_from_slice(args);
        let out = run(&temp.path, &argv);
        let code = out.status.code();
        // `impact` needs a git repository; the fixture copy has none.
        assert!(
            code == Some(0) || (new == "impact" && code == Some(1)),
            "{new}: {code:?} {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("unknown command"),
            "{new} is unknown"
        );
    }
}

/// Each old name gives the same stdout, stderr and exit code as the new name.
#[test]
fn each_old_command_name_is_byte_identical() {
    let temp = built_fixture();
    for (new, old, args) in PAIRS {
        let mut with_new = vec![new];
        with_new.extend_from_slice(args);
        let mut with_old = vec![old];
        with_old.extend_from_slice(args);
        let (a, b) = (run(&temp.path, &with_new), run(&temp.path, &with_old));
        assert_eq!(a.stdout, b.stdout, "{old} stdout");
        assert_eq!(a.stderr, b.stderr, "{old} stderr");
        assert_eq!(a.status.code(), b.status.code(), "{old} exit");
    }
}

/// `--help` lists the new names and not the old ones.
#[test]
fn help_lists_the_new_names() {
    let out = run(Path::new("."), &["--help"]);
    let text = String::from_utf8_lossy(&out.stdout);
    for new in ["query", "find", "impact", "symbols"] {
        assert!(text.contains(&format!("\n  {new} [options]")), "{new}");
    }
    for old in ["ask", "grep", "blast", "skeleton"] {
        assert!(!text.contains(&format!("\n  {old} [options]")), "{old}");
    }
}

/// The help of an old name is the help of the new name.
#[test]
fn old_name_help_is_the_new_name_help() {
    for (new, old, _) in PAIRS {
        let a = run(Path::new("."), &["help", new]);
        let b = run(Path::new("."), &["help", old]);
        assert_eq!(a.stdout, b.stdout, "help {old}");
        let c = run(Path::new("."), &[old, "--help"]);
        assert_eq!(a.stdout, c.stdout, "{old} --help");
        assert!(!a.stdout.is_empty(), "{new} help");
    }
}
