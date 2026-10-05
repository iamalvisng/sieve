//! Node-shaped file error text. Sieve prints Node's own `fs` error
//! message, for example `EACCES: permission denied, open '/x.ts'`, so a
//! Sieve build and walk print the same words (P2-01).

use std::io;
use std::path::Path;

/// Rewrites `err` as Node prints it for `syscall` on `path`.
///
/// Two kinds map to a Node code: `PermissionDenied` becomes `EACCES:
/// permission denied` and `NotFound` becomes `ENOENT: no such file or
/// directory`. Every other kind keeps its Rust text unchanged. The result
/// keeps the original [`io::ErrorKind`].
pub fn node_io_error(err: io::Error, syscall: &str, path: &Path) -> io::Error {
    let (code, text) = match err.kind() {
        io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        _ => return err,
    };
    io::Error::new(
        err.kind(),
        format!("{code}: {text}, {syscall} '{}'", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p2_01_node_io_error_maps_eacces_and_enoent() {
        let denied = node_io_error(
            io::Error::from(io::ErrorKind::PermissionDenied),
            "open",
            Path::new("/r/src/x.ts"),
        );
        assert_eq!(
            denied.to_string(),
            "EACCES: permission denied, open '/r/src/x.ts'"
        );
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);

        let missing = node_io_error(
            io::Error::from(io::ErrorKind::NotFound),
            "scandir",
            Path::new("/r/src"),
        );
        assert_eq!(
            missing.to_string(),
            "ENOENT: no such file or directory, scandir '/r/src'"
        );

        let other = node_io_error(io::Error::other("boom"), "open", Path::new("/r"));
        assert_eq!(other.to_string(), "boom");
    }
}
