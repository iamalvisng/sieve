//! The workspace index (P2-38). A parent folder with no `.git` of its own
//! and two or more git children is a workspace: `build` builds each child
//! on its own, and the parent's context dir holds only `workspace.json`.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::walk::{is_skipped_name, read_include_dirs};

/// The file name of the workspace index inside the context dir.
pub const WORKSPACE_FILE: &str = "workspace.json";

/// The workspace index: format version 1 and the child dir names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Workspace {
    /// Always 1. A file with another version reads as absent.
    pub version: u32,
    /// The child dir names, relative to the parent, sorted on write.
    pub children: Vec<String>,
}

/// The path of the workspace index for a context dir.
pub fn workspace_path(context_dir: &Path) -> PathBuf {
    context_dir.join(WORKSPACE_FILE)
}

/// Reads the index. Returns `None` when the file is absent, is not JSON,
/// has a version other than 1, or has no `children` array.
pub fn read(context_dir: &Path) -> Option<Workspace> {
    let body = std::fs::read(workspace_path(context_dir)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&body).ok()?;
    if value.get("version")?.as_u64()? != 1 {
        return None;
    }
    let children = value
        .get("children")?
        .as_array()?
        .iter()
        // Sieve maps every entry through `String(...)`.
        .map(|c| match c {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => "null".to_string(),
            other => other.to_string(),
        })
        .collect();
    Some(Workspace {
        version: 1,
        children,
    })
}

/// Writes the index with sorted children, a 2-space indent and a trailing
/// newline, as `JSON.stringify(x, null, 2) + "\n"` does. Creates the
/// context dir. Returns the path written.
pub fn write(context_dir: &Path, children: &[String]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(context_dir)?;
    let mut sorted = children.to_vec();
    sorted.sort();
    let ws = Workspace {
        version: 1,
        children: sorted,
    };
    let body = serde_json::to_string_pretty(&ws).map_err(io::Error::other)?;
    let path = workspace_path(context_dir);
    std::fs::write(&path, format!("{body}\n"))?;
    Ok(path)
}

/// Lists the direct child dirs of `root` that hold a `.git`, sorted. Skips a
/// dot dir and a `SKIP_DIRS` name, less this repo's `includeDirs` A symlink is
/// not a child: a directory check is false for a symlink, so this reads the
/// entry's own file type, which never follows a link.
pub fn discover_children(root: &Path) -> Vec<String> {
    let includes = read_include_dirs(root);
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut children: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !is_skipped_name(&e.file_name(), &includes))
        .filter(|e| e.path().join(".git").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    children.sort();
    children
}

/// Reports whether `build` at `root` is a workspace build: the index is
/// present, or `root` has no `.git` and two or more git children.
pub fn is_build_root(root: &Path, context_dir: &Path) -> bool {
    if read(context_dir).is_some() {
        return true;
    }
    if root.join(".git").exists() {
        return false;
    }
    discover_children(root).len() >= 2
}

/// The one-time split note a mega-graph parent prints on its first
/// workspace build.
pub fn migration_note(children: &[String]) -> String {
    let name = crate::product().context_dir_name();
    let dirs: Vec<String> = children.iter().map(|c| format!("{c}/{name}/")).collect();
    format!(
        "⚠ this folder contains {} separate git repos — splitting: each repo now gets its own committable {name}/ ({}); the combined graph here is replaced by a workspace index. Queries from here now search all repos, fairly.",
        children.len(),
        dirs.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sieve-workspace-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// P2-38: the index is version 1, sorted, 2-space indented, with a
    /// trailing newline; the read gives the same children back.
    #[test]
    fn test_p2_38_write_then_read_round_trips_the_sorted_index() {
        let dir = scratch("roundtrip");
        let context_dir = dir.join("sieve");
        let path = write(&context_dir, &["beta".to_string(), "alpha".to_string()]).expect("write");
        let body = std::fs::read_to_string(&path).expect("read");
        assert_eq!(
            body,
            "{\n  \"version\": 1,\n  \"children\": [\n    \"alpha\",\n    \"beta\"\n  ]\n}\n"
        );
        assert_eq!(
            read(&context_dir),
            Some(Workspace {
                version: 1,
                children: vec!["alpha".to_string(), "beta".to_string()],
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P2-38: a bad version, a bad shape, bad JSON or a missing file all
    /// read as absent.
    #[test]
    fn test_p2_38_read_treats_a_bad_index_as_absent() {
        let dir = scratch("bad");
        let context_dir = dir.join("sieve");
        std::fs::create_dir_all(&context_dir).expect("mkdir");
        assert_eq!(read(&context_dir), None);
        for body in [
            r#"{"version":2,"children":["a"]}"#,
            r#"{"version":1,"children":"a"}"#,
            r#"{"children":["a"]}"#,
            "not json",
        ] {
            std::fs::write(workspace_path(&context_dir), body).expect("write");
            assert_eq!(read(&context_dir), None, "{body}");
        }
        std::fs::write(
            workspace_path(&context_dir),
            r#"{"version":1,"children":[1,"b"]}"#,
        )
        .expect("write");
        assert_eq!(
            read(&context_dir).map(|w| w.children),
            Some(vec!["1".to_string(), "b".to_string()])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P2-38: discovery keeps a direct git child, skips a dot dir, a
    /// `SKIP_DIRS` name and a plain dir; the build-root rule needs two
    /// children and no `.git` at the root, or a present index.
    #[test]
    fn test_p2_38_discovery_and_build_root_rule() {
        let root = scratch("discover");
        for name in ["beta", "alpha", ".hidden", "node_modules", "plain"] {
            std::fs::create_dir_all(root.join(name)).expect("mkdir");
        }
        for name in ["beta", "alpha", ".hidden", "node_modules"] {
            std::fs::create_dir_all(root.join(name).join(".git")).expect("mkdir");
        }
        assert_eq!(discover_children(&root), ["alpha", "beta"]);
        let context_dir = root.join("sieve");
        assert!(is_build_root(&root, &context_dir));

        std::fs::remove_dir_all(root.join("beta")).expect("rm");
        assert!(!is_build_root(&root, &context_dir));
        write(&context_dir, &["alpha".to_string()]).expect("write");
        assert!(is_build_root(&root, &context_dir));

        std::fs::remove_dir_all(&context_dir).expect("rm");
        std::fs::create_dir_all(root.join("beta").join(".git")).expect("mkdir");
        std::fs::create_dir_all(root.join(".git")).expect("mkdir");
        assert!(!is_build_root(&root, &context_dir));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P2-38: a symlink to a git child is not a child of its own, as
    /// a directory check reads a symlink.
    #[cfg(unix)]
    #[test]
    fn test_p2_38_discovery_skips_a_symlinked_dir() {
        let root = scratch("symlink");
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(root.join(name).join(".git")).expect("mkdir");
        }
        std::os::unix::fs::symlink("alpha", root.join("gamma")).expect("symlink");
        assert_eq!(discover_children(&root), ["alpha", "beta"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P1-55: the migration note names each child's context dir.
    #[test]
    fn test_p1_55_migration_note_names_each_child() {
        let note = migration_note(&["alpha".to_string(), "beta".to_string()]);
        assert_eq!(
            note,
            "⚠ this folder contains 2 separate git repos — splitting: each repo now gets its own committable sieve/ (alpha/sieve/, beta/sieve/); the combined graph here is replaced by a workspace index. Queries from here now search all repos, fairly."
        );
    }
}
