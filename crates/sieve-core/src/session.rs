//! The latest agent session file under a context dir. `stats` reads it.

use std::path::Path;

/// The path of the newest `<context_dir>/.cache/session/*.json` file, by
/// modification time. `None` when the directory or a session file does
/// not exist.
pub fn latest_session_path(context_dir: &Path) -> Option<std::path::PathBuf> {
    let dir = context_dir.join(".cache").join("session");
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        // Strict `>`: on a tie, the first entry in directory order wins.
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn write(dir: &Path, name: &str, age_secs: u64) {
        let path = dir.join(".cache").join("session").join(name);
        fs::write(&path, "{}").unwrap();
        let when = SystemTime::now() - Duration::from_secs(age_secs);
        fs::File::open(&path).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn test_p3_39_newest_session_file_wins() {
        let dir = std::env::temp_dir().join(format!("sieve-core-session-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".cache").join("session")).unwrap();
        assert_eq!(latest_session_path(&dir), None);
        write(&dir, "old.json", 100);
        write(&dir, "new.json", 10);
        let got = latest_session_path(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(got.file_name().unwrap(), "new.json");
    }
}
