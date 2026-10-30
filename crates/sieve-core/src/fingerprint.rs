//! The freshness fingerprint. Sieve compares this record against the repo
//! on disk to find which files changed since the last build.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::walk::{walk_repo_stat, WalkEntry};

/// The version this build writes and expects to read back.
const FINGERPRINT_VERSION: u32 = 1;

/// The most recent fingerprint files Sieve keeps per context directory.
const KEPT_FINGERPRINTS: usize = 2;

/// One file's size, modified time, and content hash.
///
/// This serializes as the JSON array `[size, mtimeMs, hash]`, the same
/// the fingerprint file shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Print {
    pub size: u64,
    pub mtime_ms: f64,
    pub hash: String,
}

impl Serialize for Print {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.size, self.mtime_ms, self.hash.as_str()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Print {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (size, mtime_ms, hash) = <(u64, f64, String)>::deserialize(deserializer)?;
        Ok(Print {
            size,
            mtime_ms,
            hash,
        })
    }
}

/// The freshness record for one build: a version, an extractor stamp, and
/// a per-file print.
///
/// Sieve writes `files` sorted by path, because `BTreeMap` gives a stable order
/// for free. No parity test compares the fingerprint bytes, so the order does
/// not matter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    pub version: u32,
    pub extractor: String,
    pub files: BTreeMap<String, Print>,
    #[serde(rename = "onlyDirs", skip_serializing_if = "Option::is_none", default)]
    pub only_dirs: Option<Vec<String>>,
    /// When the build that wrote this record started its walk, in
    /// milliseconds since the epoch. The racy rule measures from here.
    #[serde(
        rename = "walkedAtMs",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub walked_at_ms: Option<f64>,
}

/// The wall-clock time now, in milliseconds since the epoch.
pub fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// Builds the fingerprint path for one context directory and stamp.
pub fn fingerprint_path(context_dir: &Path, stamp: &str) -> PathBuf {
    context_dir
        .join(".cache")
        .join(format!("fingerprint.{stamp}.json"))
}

/// Writes a fingerprint atomically, then prunes old fingerprint files.
///
/// This writes compact JSON to a temp file in the same directory, renames
/// it into place, then keeps only the newest [`KEPT_FINGERPRINTS`] files
/// whose name starts with `fingerprint.` and ends with `.json`.
pub fn write_fingerprint(path: &Path, fp: &Fingerprint) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "fingerprint path has no parent",
        )
    })?;
    fs::create_dir_all(dir)?;

    let body =
        serde_json::to_vec(fp).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    let tmp_path = dir.join(format!(".fingerprint-tmp-{}", std::process::id()));
    {
        let mut tmp = fs::File::create(&tmp_path)?;
        tmp.write_all(&body)?;
    }
    fs::rename(&tmp_path, path)?;

    prune_fingerprints(dir)
}

fn prune_fingerprints(dir: &Path) -> io::Result<()> {
    let mut entries: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("fingerprint.") && name.ends_with(".json")) {
            continue;
        }
        let metadata = entry.metadata()?;
        let mtime = metadata.modified()?;
        entries.push((entry.path(), mtime));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (path, _) in entries.into_iter().skip(KEPT_FINGERPRINTS) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

/// Reads a fingerprint back, when its version and extractor stamp match.
///
/// Returns `None` on a missing file, a parse error, a version mismatch,
/// or an extractor mismatch.
pub fn read_fingerprint(path: &Path, stamp: &str) -> Option<Fingerprint> {
    let body = fs::read(path).ok()?;
    let fp: Fingerprint = serde_json::from_slice(&body).ok()?;
    if fp.version != FINGERPRINT_VERSION || fp.extractor != stamp {
        return None;
    }
    Some(fp)
}

/// Moves a fingerprint's walk start to now, after a probe hashed every racy
/// file and found no drift (git's way to clear the racy state). The prints stay.
///
/// A racy file is safe to trust again once the clock is more than 2 s past its
/// mtime, because its content matches the record now, and any later edit gets
/// a later mtime. The check uses the system clock and the file system clock,
/// the mtime of the new file. It writes nothing and gives `false` when either
/// clock is too near the latest racy mtime.
pub fn clear_racy_state(
    path: &Path,
    fp: &Fingerprint,
    latest_racy_mtime_ms: f64,
) -> io::Result<bool> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "fingerprint path has no parent",
        )
    })?;
    clear_racy_state_at(path, fp, latest_racy_mtime_ms, now_ms(), fs_clock_ms(dir))
}

/// [`clear_racy_state`] with both clocks given, so a test can skew them.
fn clear_racy_state_at(
    path: &Path,
    fp: &Fingerprint,
    latest_racy_mtime_ms: f64,
    system_ms: f64,
    fs_ms: f64,
) -> io::Result<bool> {
    // File mtimes come from the file system clock, so that clock decides.
    // The system clock must agree, or the clocks are too far apart to trust.
    if latest_racy_mtime_ms >= system_ms - RACY_WINDOW_MS
        || latest_racy_mtime_ms >= fs_ms - RACY_WINDOW_MS
    {
        return Ok(false);
    }
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "fingerprint path has no parent",
        )
    })?;
    let mut updated = fp.clone();
    updated.walked_at_ms = Some(fs_ms);
    let body = serde_json::to_vec(&updated)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let tmp_path = dir.join(format!(".fingerprint-tmp-{}", std::process::id()));
    fs::write(&tmp_path, &body)?;
    fs::rename(&tmp_path, path)?;
    Ok(true)
}

/// The walk start a build records in its fingerprint, from the file system
/// clock of the build's cache dir. Both build paths call this before they walk.
pub fn walk_start_ms(context_dir: &Path) -> f64 {
    let dir = context_dir.join(".cache");
    let _ = fs::create_dir_all(&dir);
    fs_clock_ms(&dir)
}

/// Reads the file system clock, in milliseconds, as the mtime of a file made
/// now in `dir`. File mtimes come from this clock, and the racy rule compares
/// them with the walk start, so a build must take the walk start from here.
/// A file system clock that runs behind the system clock would hide an edit
/// otherwise. It falls back to the system clock when the dir takes no file.
pub fn fs_clock_ms(dir: &Path) -> f64 {
    let probe = dir.join(format!(".clock-{}", std::process::id()));
    let mtime = fs::write(&probe, b"")
        .ok()
        .and_then(|()| fingerprint_mtime_ms(&probe));
    let _ = fs::remove_file(&probe);
    mtime.unwrap_or_else(now_ms)
}

/// Reads one file's size and modified time, in milliseconds, from disk.
pub fn stat_print(root: &Path, rel: &str) -> io::Result<(u64, f64)> {
    metadata_print(&fs::metadata(root.join(rel))?)
}

/// Reads the size and mtime, in milliseconds, from one file's metadata.
pub(crate) fn metadata_print(metadata: &fs::Metadata) -> io::Result<(u64, f64)> {
    let size = metadata.len();
    let modified = metadata.modified()?;
    let duration = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mtime_ms =
        duration.as_secs() as f64 * 1000.0 + f64::from(duration.subsec_nanos()) / 1_000_000.0;
    Ok((size, mtime_ms))
}

/// Hashes source bytes to a lower-case sha256 hex string.
pub fn hash_source(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Decodes a source file's raw bytes to text.
///
/// Every build, refresh, `check`, `grep` and `ask --source` read goes through
/// this decode, so a byte that is not valid UTF-8 (for example a Latin-1
/// `0xE9`) never drops the file. The build hashes the decoded text, so a drift
/// probe must decode the same way before it hashes.
///
/// - A UTF-16BE BOM (`0xFE 0xFF`) returns `None`: Node has no built-in
///   UTF-16BE decoder, so the file is unreadable. The caller
///   must skip it, the same as a read error, not hash it as UTF-8.
/// - A UTF-16LE BOM (`0xFF 0xFE`) decodes the bytes after the BOM as
///   UTF-16LE text.
/// - Anything else (including a UTF-8 BOM) decodes as UTF-8 text, with no
///   BOM stripped, matching `Buffer.toString("utf8")`. An invalid byte
///   sequence becomes U+FFFD, one per maximal subpart, as WHATWG decodes
///   it and as `String::from_utf8_lossy` does.
///
/// Sieve's UTF-16LE decode is lossy on an unpaired surrogate (Node's
/// decoder replaces it with U+FFFD, same as `char::decode_utf16`), so this
/// never fails.
pub fn decode_source(bytes: &[u8]) -> Option<String> {
    if bytes.first() == Some(&0xfe) && bytes.get(1) == Some(&0xff) {
        return None;
    }
    if bytes.first() == Some(&0xff) && bytes.get(1) == Some(&0xfe) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        return Some(
            char::decode_utf16(units)
                .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect(),
        );
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// The set of relative paths that changed since a fingerprint was taken.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drift {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

/// How close, in milliseconds, a file's recorded mtime may sit to the write of
/// the fingerprint before the probe hashes the file in place of trusting its
/// stat. This is git's racy rule: an edit after the build's stat can keep the
/// same size and the same mtime tick.
const RACY_WINDOW_MS: f64 = 2000.0;

/// Reads a fingerprint file's own mtime in milliseconds, for the racy rule.
pub fn fingerprint_mtime_ms(path: &Path) -> Option<f64> {
    metadata_print(&fs::metadata(path).ok()?)
        .ok()
        .map(|(_, ms)| ms)
}

/// What stat-ing one recorded file against its print gives back.
enum StatOutcome {
    /// The current size and mtime, to compare against the record.
    Stat(u64, f64),
    /// The record already counts as changed; skip the read.
    Changed,
    /// Neither a stat nor a verdict: skip this file.
    Skip,
}

/// Stats one recorded file, classifying a stat failure the way
///  classifies a failed read: a file gone by stat
/// time counts as `changed` when the record's hash is non-empty; any other
/// error is skipped. A file `walk_repo` listed (for example a git-tracked
/// file deleted from disk) can still vanish before this stat runs.
fn stat_or_classify(root: &Path, rel_str: &str, record: &Print) -> StatOutcome {
    match stat_print(root, rel_str) {
        Ok((size, mtime_ms)) => StatOutcome::Stat(size, mtime_ms),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if record.hash.is_empty() {
                StatOutcome::Skip
            } else {
                StatOutcome::Changed
            }
        }
        Err(_) => StatOutcome::Skip,
    }
}

/// Reports whether a repo-relative posix path sits at or under one of
/// `only_dirs`. An empty list accepts every path.
pub fn under_only_dirs(rel: &str, only_dirs: &[String]) -> bool {
    only_dirs.is_empty()
        || only_dirs
            .iter()
            .any(|d| rel == d || rel.starts_with(&format!("{d}/")))
}

/// Compares a fingerprint against the repo on disk and reports the drift.
///
/// This walks the repo with [`walk_repo`] and keeps only the files `claims`
/// accepts. A caller must pass the same claim rule a build uses to fingerprint
/// files (native, container, and breadth tiers), or every breadth-tier file in
/// `fp.files` reads as `removed` on every call, because it never reaches `seen`
/// (P1-46, P2-35). A record's hash is trusted, with no read, when `force_hash`
/// is false and the size and `mtime_ms` both still match. A fingerprint with
/// `onlyDirs` probes only the files under those prefixes, so a `build
/// --only-dir` graph never reads as drifted by the files it left out (P1-70).
pub fn probe_drift(
    root: &Path,
    fp: &Fingerprint,
    force_hash: bool,
    claims: impl Fn(&Path) -> bool,
) -> io::Result<Drift> {
    probe_drift_walk(root, fp, force_hash, None, claims).map(|(drift, _)| drift)
}

/// Like [`probe_drift`], and also returns the full walk list, so a rebuild
/// does not walk again. The probe takes each file's size and mtime from the
/// walk's own `lstat`. The racy rule measures from the walk start that the
/// fingerprint records. An old record has none, so `fp_written_ms`, the mtime
/// of the fingerprint file, stands in.
/// A file whose recorded mtime sits within 2 s of it, or after it, is hashed,
/// not trusted by its stat. `None` turns this racy rule off.
pub fn probe_drift_walk(
    root: &Path,
    fp: &Fingerprint,
    force_hash: bool,
    fp_written_ms: Option<f64>,
    claims: impl Fn(&Path) -> bool,
) -> io::Result<(Drift, Vec<PathBuf>)> {
    probe_drift_racy(root, fp, force_hash, fp_written_ms, claims)
        .map(|(drift, files, _)| (drift, files))
}

/// What the racy rule did in one probe.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RacyInfo {
    /// The files the probe hashed only because of the racy rule.
    pub hashed: usize,
    /// The latest recorded mtime among those files.
    pub latest_mtime_ms: f64,
    /// True when a racy file could not be read or decoded.
    pub unverified: bool,
}

/// Like [`probe_drift_walk`], and also reports the racy rule's work.
pub fn probe_drift_racy(
    root: &Path,
    fp: &Fingerprint,
    force_hash: bool,
    fp_written_ms: Option<f64>,
    claims: impl Fn(&Path) -> bool,
) -> io::Result<(Drift, Vec<PathBuf>, RacyInfo)> {
    let mut info = RacyInfo::default();
    let files = walk_repo_stat(root)?;
    let reference_ms = fp.walked_at_ms.or(fp_written_ms);
    let only_dirs = fp.only_dirs.as_deref().unwrap_or(&[]);
    let mut seen = std::collections::BTreeSet::new();
    let mut drift = Drift::default();

    for WalkEntry { rel, stat } in &files {
        if !under_only_dirs(&rel.to_string_lossy().replace('\\', "/"), only_dirs) {
            continue;
        }
        if !claims(rel) {
            continue;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        seen.insert(rel_str.clone());

        let Some(record) = fp.files.get(&rel_str) else {
            drift.added.push(rel_str);
            continue;
        };

        let walked_stat = stat.map(|(size, mtime_ms)| StatOutcome::Stat(size, mtime_ms));
        let (size, mtime_ms) =
            match walked_stat.unwrap_or_else(|| stat_or_classify(root, &rel_str, record)) {
                StatOutcome::Stat(size, mtime_ms) => (size, mtime_ms),
                StatOutcome::Changed => {
                    drift.changed.push(rel_str);
                    continue;
                }
                StatOutcome::Skip => continue,
            };

        let racy = reference_ms.is_some_and(|at| record.mtime_ms > at - RACY_WINDOW_MS);
        let stat_unchanged = !force_hash
            && !racy
            && !record.hash.is_empty()
            && record.size == size
            && record.mtime_ms == mtime_ms;
        if stat_unchanged {
            continue;
        }
        let racy_only = racy
            && !force_hash
            && !record.hash.is_empty()
            && record.size == size
            && record.mtime_ms == mtime_ms;
        if racy_only {
            info.hashed += 1;
            info.latest_mtime_ms = info.latest_mtime_ms.max(record.mtime_ms);
        }

        let bytes = match fs::read(root.join(&rel_str)) {
            Ok(bytes) => bytes,
            Err(_) => {
                info.unverified |= racy_only;
                continue;
            }
        };
        // Hash the decoded text, and a file that stops decoding is drift only
        // when the last build read it (a non-empty hash).
        let Some(text) = decode_source(&bytes) else {
            info.unverified |= racy_only;
            if !record.hash.is_empty() {
                drift.changed.push(rel_str);
            }
            continue;
        };
        let hash = hash_source(text.as_bytes());
        if hash != record.hash {
            drift.changed.push(rel_str);
        }
    }

    for rel_str in fp.files.keys() {
        if !seen.contains(rel_str) {
            drift.removed.push(rel_str.clone());
        }
    }

    drift.added.sort();
    drift.changed.sort();
    drift.removed.sort();
    Ok((drift, files.into_iter().map(|e| e.rel).collect(), info))
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
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("sieve-fingerprint-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn fp_for(root: &Path, rel: &str) -> Fingerprint {
        let bytes = fs::read(root.join(rel)).expect("read file");
        let (size, mtime_ms) = stat_print(root, rel).expect("stat file");
        let mut files = BTreeMap::new();
        files.insert(
            rel.to_string(),
            Print {
                size,
                mtime_ms,
                hash: hash_source(&bytes),
            },
        );
        Fingerprint {
            version: FINGERPRINT_VERSION,
            extractor: "stamp1".to_string(),
            files,
            only_dirs: None,
            walked_at_ms: None,
        }
    }

    /// P1-70: the `onlyDirs` prefix match is segment-aware, and an empty
    /// list accepts every path.
    #[test]
    fn test_p1_70_under_only_dirs_matches_whole_segments() {
        let only = vec!["src".to_string()];
        assert!(under_only_dirs("src", &only));
        assert!(under_only_dirs("src/a.ts", &only));
        assert!(!under_only_dirs("srcx/a.ts", &only));
        assert!(!under_only_dirs("py/a.py", &only));
        assert!(under_only_dirs("py/a.py", &[]));
    }

    #[test]
    fn print_round_trips_as_a_json_array() {
        let print = Print {
            size: 12,
            mtime_ms: 1234.5,
            hash: "abcd".to_string(),
        };
        let json = serde_json::to_string(&print).expect("serialize");
        assert_eq!(json, "[12,1234.5,\"abcd\"]");
        let back: Print = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, print);
    }

    #[test]
    fn stat_print_gives_the_same_mtime_ms_twice_for_one_file() {
        // Ponytail: pins the P2-35 flake. `stat_print` reads the file's
        // real mtime twice; both reads must give the exact same `f64`, or
        // a warm build treats an unchanged file as stale.
        let dir = TempDir::new("stat-twice");
        fs::write(dir.path.join("a.ts"), b"const a = 1;").expect("write file");
        let (size_a, mtime_a) = stat_print(&dir.path, "a.ts").expect("stat file first time");
        let (size_b, mtime_b) = stat_print(&dir.path, "a.ts").expect("stat file second time");
        assert_eq!(size_a, size_b);
        assert_eq!(mtime_a, mtime_b);
    }

    #[test]
    fn print_mtime_ms_round_trips_exactly_through_json() {
        // Ponytail: pins the P2-35 flake. Without `serde_json`'s
        // `float_roundtrip` feature, some 16-significant-digit `mtime_ms`
        // values (real mtimes look like this) come back one ULP off,
        // so the cache treats an unchanged file as stale.
        let flaky_values = [1789227231000.0015_f64, 1789227231802.276_f64];
        for mtime_ms in flaky_values {
            let print = Print {
                size: 3,
                mtime_ms,
                hash: "deadbeef".to_string(),
            };
            let json = serde_json::to_string(&print).expect("serialize");
            let back: Print = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back.mtime_ms, mtime_ms, "json was {json}");
        }
    }

    #[test]
    fn write_then_read_round_trips_when_the_stamp_matches() {
        let dir = TempDir::new("roundtrip");
        let path = fingerprint_path(&dir.path, "stamp1");
        let mut files = BTreeMap::new();
        files.insert(
            "a.ts".to_string(),
            Print {
                size: 3,
                mtime_ms: 1.0,
                hash: "deadbeef".to_string(),
            },
        );
        let fp = Fingerprint {
            version: FINGERPRINT_VERSION,
            extractor: "stamp1".to_string(),
            files,
            only_dirs: None,
            walked_at_ms: None,
        };
        write_fingerprint(&path, &fp).expect("write fingerprint");
        let back = read_fingerprint(&path, "stamp1").expect("read fingerprint");
        assert_eq!(back, fp);
    }

    #[test]
    fn read_fingerprint_rejects_a_version_or_stamp_mismatch() {
        let dir = TempDir::new("mismatch");
        let path = fingerprint_path(&dir.path, "stamp1");
        let fp = Fingerprint {
            version: FINGERPRINT_VERSION,
            extractor: "stamp1".to_string(),
            files: BTreeMap::new(),
            only_dirs: None,
            walked_at_ms: None,
        };
        write_fingerprint(&path, &fp).expect("write fingerprint");

        assert!(read_fingerprint(&path, "other-stamp").is_none());

        let bad_version = Fingerprint { version: 2, ..fp };
        write_fingerprint(&path, &bad_version).expect("write fingerprint");
        assert!(read_fingerprint(&path, "stamp1").is_none());
    }

    #[test]
    fn prune_keeps_only_the_two_newest_fingerprints() {
        let dir = TempDir::new("prune");
        let cache_dir = dir.path.join(".cache");
        fs::create_dir_all(&cache_dir).expect("create cache dir");
        let fp = Fingerprint {
            version: FINGERPRINT_VERSION,
            extractor: "s".to_string(),
            files: BTreeMap::new(),
            only_dirs: None,
            walked_at_ms: None,
        };
        for label in ["a", "b", "c"] {
            let path = cache_dir.join(format!("fingerprint.{label}.json"));
            write_fingerprint(&path, &fp).expect("write fingerprint");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let remaining: Vec<_> = fs::read_dir(&cache_dir)
            .expect("read cache dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("fingerprint.") && name.ends_with(".json")
            })
            .collect();
        assert_eq!(remaining.len(), KEPT_FINGERPRINTS);
    }

    #[test]
    fn probe_drift_reports_added_changed_and_removed() {
        let dir = TempDir::new("drift");
        fs::write(dir.path.join("a.ts"), b"const a = 1;").expect("write a.ts");
        fs::write(dir.path.join("b.ts"), b"const b = 2;").expect("write b.ts");

        let mut fp = fp_for(&dir.path, "a.ts");
        let b_bytes = fs::read(dir.path.join("b.ts")).expect("read b.ts");
        fp.files.insert(
            "gone.ts".to_string(),
            Print {
                size: 1,
                mtime_ms: 1.0,
                hash: hash_source(b"stale"),
            },
        );
        // b.ts is on disk but absent from the record, so it reads as added.
        let _ = b_bytes;

        let drift = probe_drift(&dir.path, &fp, false, |_: &Path| true).expect("probe drift");
        assert_eq!(drift.added, vec!["b.ts".to_string()]);
        assert_eq!(drift.removed, vec!["gone.ts".to_string()]);
        assert!(drift.changed.is_empty());
    }

    #[test]
    fn probe_drift_skips_the_read_when_stat_matches() {
        let dir = TempDir::new("clean");
        fs::write(dir.path.join("a.ts"), b"const a = 1;").expect("write a.ts");
        let fp = fp_for(&dir.path, "a.ts");

        let drift = probe_drift(&dir.path, &fp, false, |_: &Path| true).expect("probe drift");
        assert!(drift.added.is_empty());
        assert!(drift.changed.is_empty());
        assert!(drift.removed.is_empty());
    }

    #[test]
    fn stat_or_classify_reports_changed_for_a_missing_file_with_a_recorded_hash() {
        let dir = TempDir::new("missing-with-hash");
        let record = Print {
            size: 1,
            mtime_ms: 1.0,
            hash: hash_source(b"stale"),
        };

        // "gone.ts" was never created: a deterministic stand-in for a file
        // that vanished between the walk and this stat.
        match stat_or_classify(&dir.path, "gone.ts", &record) {
            StatOutcome::Changed => {}
            _ => panic!("expected StatOutcome::Changed"),
        }
    }

    #[test]
    fn stat_or_classify_skips_a_missing_file_with_no_recorded_hash() {
        let dir = TempDir::new("missing-no-hash");
        let record = Print {
            size: 1,
            mtime_ms: 1.0,
            hash: String::new(),
        };

        match stat_or_classify(&dir.path, "gone.ts", &record) {
            StatOutcome::Skip => {}
            _ => panic!("expected StatOutcome::Skip"),
        }
    }

    #[test]
    fn probe_drift_finds_a_same_size_rewrite_only_with_force_hash() {
        let dir = TempDir::new("rewrite");
        fs::write(dir.path.join("a.ts"), b"const a = 1;").expect("write a.ts");
        let fp = fp_for(&dir.path, "a.ts");

        // Same byte length, different content, same mtime as the record.
        // Capture the exact `SystemTime`, not a reconstruction from the
        // float `mtime_ms`, so no sub-millisecond precision is lost.
        let original_mtime = fs::metadata(dir.path.join("a.ts"))
            .expect("stat file")
            .modified()
            .expect("modified time");
        fs::write(dir.path.join("a.ts"), b"const a = 9;").expect("rewrite a.ts");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.path.join("a.ts"))
            .expect("open a.ts");
        let _ = file.set_modified(original_mtime);

        let clean = probe_drift(&dir.path, &fp, false, |_: &Path| true).expect("probe drift");
        assert!(clean.changed.is_empty());

        let forced = probe_drift(&dir.path, &fp, true, |_: &Path| true).expect("probe drift");
        assert_eq!(forced.changed, vec!["a.ts".to_string()]);
    }

    /// A same-size rewrite that keeps the mtime hides from the stat. The racy
    /// rule hashes a file whose record sits within 2 s of the fingerprint write.
    #[test]
    fn probe_drift_walk_hashes_a_racy_file_and_finds_the_rewrite() {
        let dir = TempDir::new("racy");
        let a = dir.path.join("a.ts");
        fs::write(&a, b"const a = 1;").expect("write a.ts");
        let fp = fp_for(&dir.path, "a.ts");
        let fp_file = dir.path.join("fp.json");
        write_fingerprint(&fp_file, &fp).expect("write fingerprint");
        let original = fs::metadata(&a).expect("stat").modified().expect("mtime");
        fs::write(&a, b"const a = 9;").expect("rewrite a.ts");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&a)
            .expect("open a.ts");
        file.set_times(fs::FileTimes::new().set_modified(original))
            .expect("restore mtime");

        let written = fingerprint_mtime_ms(&fp_file);
        assert!(written.is_some());
        let (racy, _) =
            probe_drift_walk(&dir.path, &fp, false, written, |_: &Path| true).expect("probe");
        assert_eq!(racy.changed, vec!["a.ts".to_string()]);
        // With the rule off, the stat matches and the rewrite hides.
        let (blind, _) =
            probe_drift_walk(&dir.path, &fp, false, None, |_: &Path| true).expect("probe");
        assert!(blind.changed.is_empty());
    }

    /// The probe returns the full walk list, in walk order.
    #[test]
    fn probe_drift_walk_returns_the_walk_list() {
        let dir = TempDir::new("list");
        fs::write(dir.path.join("a.ts"), b"x").expect("write a.ts");
        fs::write(dir.path.join("b.ts"), b"y").expect("write b.ts");
        let fp = fp_for(&dir.path, "a.ts");
        let (_, files) =
            probe_drift_walk(&dir.path, &fp, false, None, |_: &Path| true).expect("probe");
        assert_eq!(files, vec![PathBuf::from("a.ts"), PathBuf::from("b.ts")]);
    }

    /// A build that takes longer than 2 s writes the fingerprint file late. The
    /// racy rule measures from the walk start, so a same-size rewrite after the
    /// stat still shows. The fingerprint file mtime alone would miss it.
    #[test]
    fn probe_drift_walk_racy_rule_uses_the_walk_start_not_the_fingerprint_write() {
        let dir = TempDir::new("racy-slow-build");
        let a = dir.path.join("a.ts");
        fs::write(&a, b"const a = 1;").expect("write a.ts");
        let mut fp = fp_for(&dir.path, "a.ts");
        let record_ms = fp.files["a.ts"].mtime_ms;
        let original = fs::metadata(&a).expect("stat").modified().expect("mtime");
        fs::write(&a, b"const a = 9;").expect("rewrite a.ts");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&a)
            .expect("open a.ts");
        file.set_times(fs::FileTimes::new().set_modified(original))
            .expect("restore mtime");

        // The walk started 100 ms after the file's mtime. The build took 10 s.
        let late_write = Some(record_ms + 10_000.0);
        fp.walked_at_ms = Some(record_ms + 100.0);
        let (found, _) =
            probe_drift_walk(&dir.path, &fp, false, late_write, |_: &Path| true).expect("probe");
        assert_eq!(found.changed, vec!["a.ts".to_string()]);

        // An old record has no walk start, so the late write stands in, and misses.
        fp.walked_at_ms = None;
        let (missed, _) =
            probe_drift_walk(&dir.path, &fp, false, late_write, |_: &Path| true).expect("probe");
        assert!(missed.changed.is_empty());
    }

    /// The file system clock reads near the system clock, and leaves no file.
    #[test]
    fn fs_clock_ms_reads_the_mtime_of_a_new_file_and_leaves_nothing() {
        let dir = TempDir::new("fs-clock");
        let before = now_ms();
        let fs_ms = fs_clock_ms(&dir.path);
        assert!((fs_ms - before).abs() < 2000.0, "{fs_ms} vs {before}");
        assert_eq!(fs::read_dir(&dir.path).expect("list").count(), 0);
        // A dir that takes no file falls back to the system clock.
        let missing = dir.path.join("no-such-dir");
        assert!((fs_clock_ms(&missing) - now_ms()).abs() < 2000.0);
    }

    /// A file system clock 10 s behind the system clock decides. The record
    /// takes the file system time, and a racy file near it is not cleared.
    #[test]
    fn clear_racy_state_follows_the_file_system_clock_when_it_runs_behind() {
        let dir = TempDir::new("skew");
        let path = dir.path.join("fingerprint.s.json");
        let fp = Fingerprint {
            version: FINGERPRINT_VERSION,
            extractor: "s".to_string(),
            files: BTreeMap::new(),
            only_dirs: None,
            walked_at_ms: None,
        };
        let system = 1_000_000.0;
        let fs_behind = system - 10_000.0;
        // The file mtime sits 1 s before the file system clock: still racy.
        let near = fs_behind - 1000.0;
        assert!(!clear_racy_state_at(&path, &fp, near, system, fs_behind).expect("clear"));
        assert!(!path.exists());
        // 3 s before the file system clock: clear, stamped with that clock.
        let old = fs_behind - 3000.0;
        assert!(clear_racy_state_at(&path, &fp, old, system, fs_behind).expect("clear"));
        let back: Fingerprint =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("decode");
        assert_eq!(back.walked_at_ms, Some(fs_behind));
    }
}
