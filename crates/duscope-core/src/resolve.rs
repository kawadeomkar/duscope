//! The one place a path from a user or an agent becomes the path duscope
//! reads: relative to the working directory, symbolic links resolved. No
//! other code calls `canonicalize`.
//!
//! The index adds a lookup with case and normalisation folding in phase 2
//! (P2-QRY-01); until then a path resolves on the filesystem alone.

use std::path::{Path, PathBuf};

/// A path as it was given, and the path it leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The path, absolute, with every symbolic link resolved. A path that
    /// leads nowhere (it does not exist, or ends in a link to nothing) stays
    /// as given, made absolute, without `.` components: what reads it then
    /// reports the error, or reads the dangling link itself.
    pub real: PathBuf,
    /// The path as given.
    pub as_given: PathBuf,
}

/// Resolves `given`, read relative to `cwd` when it is relative.
pub fn resolve_path(cwd: &Path, given: &Path) -> ResolvedPath {
    let absolute: PathBuf = cwd.join(given).components().collect();
    let real = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    ResolvedPath {
        real,
        as_given: given.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use duscope_testkit::TestEnv;

    use super::resolve_path;

    #[test]
    fn p1_cli_02_resolve_path_follows_links_and_keeps_the_path_as_given() {
        let env = TestEnv::new();
        let links = fs::canonicalize(env.fixtures().case_root("symlinks")).unwrap();
        let real_dir = links.join("real-dir");
        // An absolute path through a link, and a relative one.
        let resolved = resolve_path(Path::new("/"), &links.join("to-dir"));
        assert_eq!(resolved.real, real_dir);
        assert_eq!(resolved.as_given, links.join("to-dir"));
        let resolved = resolve_path(&links, Path::new("to-dir"));
        assert_eq!(resolved.real, real_dir);
        assert_eq!(resolved.as_given, Path::new("to-dir"));
        // A link to a file leads to the file.
        let resolved = resolve_path(&links, Path::new("to-file"));
        assert_eq!(resolved.real, links.join("target.txt"));
        // A dangling link, or a path that does not exist, stays as given,
        // made absolute.
        let resolved = resolve_path(&links, Path::new("./dangling"));
        assert_eq!(resolved.real, links.join("dangling"));
        assert_eq!(resolved.as_given, Path::new("./dangling"));
        let resolved = resolve_path(&links, Path::new("missing/./x"));
        assert_eq!(resolved.real, links.join("missing/x"));
    }
}
