//! The envelope's `next`: up to three runnable commands for the same scan,
//! most useful first. Each names the scan so that it reads the scan the
//! envelope came from however many are saved after: by its id, with
//! `--index-dir` where a command without it would look elsewhere, or by
//! its index file outside an index directory. Each asks for `--json`, as
//! the envelope it rides in was. A scan that is not saved has no name, and
//! no hints. Where rows remain, the next page comes first: the command
//! that ran, on the absolute path, with `--cursor`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use duscope_core::envelope::{
    ByTypeEntry, CacheEntry, Envelope, ProjectEntry, StaleEntry, TopEntry, TreeEntry,
};

use super::treemap::shell_quote;

/// The most hints an envelope carries.
const MAX: usize = 3;

/// A row a hint can point at.
pub trait Hinted {
    /// The path worth `explain`ing, if the row names one.
    fn hint_path(&self) -> Option<&str>;

    /// Whether the rows are `caches --by-project`'s.
    fn by_project() -> bool {
        false
    }
}

impl Hinted for TopEntry {
    fn hint_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}

impl Hinted for TreeEntry {
    fn hint_path(&self) -> Option<&str> {
        // The root is the path the tree was asked about.
        (self.depth > 0).then_some(self.path.as_str())
    }
}

impl Hinted for ByTypeEntry {
    fn hint_path(&self) -> Option<&str> {
        self.largest.first().map(|example| example.path.as_str())
    }
}

impl Hinted for StaleEntry {
    fn hint_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}

impl Hinted for CacheEntry {
    fn hint_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}

impl Hinted for ProjectEntry {
    fn hint_path(&self) -> Option<&str> {
        self.project_root.as_deref()
    }

    fn by_project() -> bool {
        true
    }
}

/// The hints being gathered for a scan.
#[derive(Debug)]
pub struct Hints {
    /// The flags that name the scan.
    scan: Vec<String>,
    hints: Vec<String>,
}

impl Hints {
    /// Hints for the saved scan `id`, whose index is `file`; `None` for a
    /// scan not saved.
    pub fn new(id: Option<&str>, file: Option<&Path>) -> Option<Hints> {
        Some(Hints {
            scan: selector(id?, file?),
            hints: Vec::new(),
        })
    }

    /// The next page, when `cursor` says rows remain: the command that
    /// ran, with `path`, absolute, for the path it was given (`given`;
    /// the working directory when none was), this scan, and `cursor`. A
    /// command line that is not UTF-8 gets no hint, nor do `-o`'s file
    /// (the hint would write over it) and the flags the hint sets.
    pub fn page(
        &mut self,
        cursor: Option<&str>,
        verb: &str,
        path: &str,
        given: Option<&Path>,
    ) -> &mut Hints {
        let Some(cursor) = cursor else {
            return self;
        };
        let Some(ran) = std::env::args_os()
            .skip(1)
            .map(|a| a.into_string().ok())
            .collect::<Option<Vec<String>>>()
        else {
            return self;
        };
        let given = given.and_then(|p| p.to_str());
        let mut args: Vec<String> = Vec::new();
        let mut placed = false;
        let mut ran = ran.into_iter();
        while let Some(arg) = ran.next() {
            match arg.as_str() {
                "--cursor" | "--scan" | "--index-dir" | "-o" | "--output" => {
                    ran.next();
                }
                _ if ["--cursor=", "--scan=", "--index-dir=", "--output="]
                    .iter()
                    .any(|p| arg.starts_with(p))
                    || (arg.starts_with("-o") && arg.len() > 2) => {}
                _ if !placed && Some(arg.as_str()) == given => {
                    args.push(path.to_owned());
                    placed = true;
                }
                _ if !placed && given.is_none() && arg == verb => {
                    args.extend([arg, path.to_owned()]);
                    placed = true;
                }
                _ => args.push(arg),
            }
        }
        args.extend(self.scan.iter().cloned());
        args.extend(["--cursor".to_owned(), cursor.to_owned()]);
        self.hints.push(line(args.iter().map(String::as_str)));
        self
    }

    /// `duscope <verb> <path> <flags> <the scan> --json`. A path that is
    /// not UTF-8 (read lossily) names no file, and gets no hint.
    pub fn run(&mut self, verb: &str, path: Option<&str>, flags: &[&str]) -> &mut Hints {
        let Some(path) = path.filter(|p| !p.contains('\u{fffd}')) else {
            return self;
        };
        let mut args = vec![verb, path];
        args.extend(flags);
        args.extend(self.scan.iter().map(String::as_str));
        args.push("--json");
        self.hints.push(line(args));
        self
    }

    /// `duscope explain --errors <the scan> --json`: `--errors` takes the
    /// scan `--scan` would.
    pub fn errors(&mut self) -> &mut Hints {
        let mut args = vec!["explain"];
        for arg in &self.scan {
            args.push(if arg == "--scan" { "--errors" } else { arg });
        }
        args.push("--json");
        self.hints.push(line(args));
        self
    }

    /// The hints, the first three.
    pub fn done(&mut self) -> Vec<String> {
        let mut hints = std::mem::take(&mut self.hints);
        hints.truncate(MAX);
        hints
    }
}

/// The flags that name scan `id` in `file`: `--scan <id>` for an index in
/// the directory a command reads without `--index-dir`, with
/// `--index-dir` for one in another, and `--scan <file>` for a file
/// outside the `scans/<id>.db` layout.
fn selector(id: &str, file: &Path) -> Vec<String> {
    let in_layout = file.file_name() == Some(OsStr::new(&format!("{id}.db")))
        && file.parent().and_then(Path::file_name) == Some(OsStr::new("scans"));
    let dir = file.parent().and_then(Path::parent).filter(|_| in_layout);
    match dir {
        Some(dir) if Some(dir) == plain_dir().as_deref() => {
            vec!["--scan".to_owned(), id.to_owned()]
        }
        Some(dir) => vec![
            "--index-dir".to_owned(),
            dir.to_string_lossy().into_owned(),
            "--scan".to_owned(),
            id.to_owned(),
        ],
        None => vec!["--scan".to_owned(), file.to_string_lossy().into_owned()],
    }
}

/// The index directory a command without `--index-dir` reads.
fn plain_dir() -> Option<PathBuf> {
    std::env::var_os("DUSCOPE_INDEX_DIR")
        .map(PathBuf::from)
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
}

/// The hints of a list verb's envelope, by its verb: the scan is in
/// `file`, and the verb was given `given`.
pub fn list<T: Hinted>(e: &Envelope<T>, file: Option<&Path>, given: Option<&Path>) -> Vec<String> {
    let Some(mut hints) = Hints::new(e.scan.id.as_deref(), file) else {
        return Vec::new();
    };
    let path = Some(e.query.path.as_str());
    let first = e.entries.iter().find_map(Hinted::hint_path);
    hints.page(
        e.next_cursor.as_deref(),
        e.query.command,
        &e.query.path,
        given,
    );
    match e.query.command {
        "scan" | "rescan" | "import" => {
            hints
                .run("top", path, &[])
                .run("by-type", path, &[])
                .run("caches", path, &[])
        }
        "top" | "stale" => hints.run("explain", first, &[]).run("by-type", path, &[]),
        "tree" => hints
            .run("explain", first, &[])
            .run("top", path, &["--files"]),
        "by-type" => hints
            .run("explain", first, &[])
            .run("top", path, &["--files"])
            .run("caches", path, &[]),
        "caches" if T::by_project() => hints.run("explain", first, &[]).run("caches", path, &[]),
        "caches" => hints
            .run("explain", first, &[])
            .run("caches", path, &["--by-project"]),
        _ => &mut hints,
    };
    hints.done()
}

/// `args` as one command line for a POSIX shell, after `duscope`.
fn line<'a>(args: impl IntoIterator<Item = &'a str>) -> String {
    let mut line = String::from("duscope");
    for arg in args {
        line.push(' ');
        line.push_str(&shell_quote(arg));
    }
    line
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Hints, line, selector};

    #[test]
    fn p2_cli_01_a_hint_is_a_runnable_command_line() {
        assert_eq!(line(["top", "/a b"]), "duscope top '/a b'");
        let file = Path::new("/elsewhere/scans/3f9a0c1d2e3f.db");
        let mut hints = Hints::new(Some("3f9a0c1d2e3f"), Some(file)).unwrap();
        let got = hints
            .run("explain", Some("/r/x"), &[])
            .run("top", Some("/r"), &["--files"])
            .run("explain", Some("/r/\u{fffd}"), &[])
            .errors()
            .run("stale", Some("/r"), &[])
            .done();
        assert_eq!(
            got,
            [
                "duscope explain /r/x --index-dir /elsewhere --scan 3f9a0c1d2e3f --json",
                "duscope top /r --files --index-dir /elsewhere --scan 3f9a0c1d2e3f --json",
                "duscope explain --index-dir /elsewhere --errors 3f9a0c1d2e3f --json",
            ]
        );
        assert!(Hints::new(None, Some(file)).is_none());
        assert!(Hints::new(Some("3f9a0c1d2e3f"), None).is_none());
        // An index file outside the layout is named by its path.
        assert_eq!(
            selector("3f9a0c1d2e3f", Path::new("/tmp/copy.db")),
            ["--scan", "/tmp/copy.db"]
        );
    }
}
