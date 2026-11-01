//! The envelope's `next`: up to three runnable commands for the same scan,
//! most useful first. Each names the scan by its id, so it reads the scan
//! the envelope came from however many are saved after, and asks for
//! `--json`, as the envelope it rides in was. A scan that is not saved has
//! no id to name, and no hints. Where rows remain, the next page comes
//! first: the command that ran, with `--cursor`.

use std::borrow::Cow;

use duscope_core::envelope::{
    ByTypeEntry, CacheEntry, Envelope, ProjectEntry, StaleEntry, TopEntry, TreeEntry,
};

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
    scan: String,
    hints: Vec<String>,
}

impl Hints {
    /// Hints for the scan `scan`; `None` for one not saved.
    pub fn new(scan: Option<&str>) -> Option<Hints> {
        Some(Hints {
            scan: scan?.to_owned(),
            hints: Vec::new(),
        })
    }

    /// The next page, when `cursor` says rows remain: the command that
    /// ran, reading this scan, from `cursor`.
    pub fn page(&mut self, cursor: Option<&str>) -> &mut Hints {
        let Some(cursor) = cursor else {
            return self;
        };
        let mut args: Vec<String> = Vec::new();
        let mut given = std::env::args_os()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned());
        while let Some(arg) = given.next() {
            match arg.as_str() {
                "--cursor" | "--scan" => {
                    given.next();
                }
                _ if arg.starts_with("--cursor=") || arg.starts_with("--scan=") => {}
                _ => args.push(arg),
            }
        }
        args.extend([
            "--scan".to_owned(),
            self.scan.clone(),
            "--cursor".to_owned(),
            cursor.to_owned(),
        ]);
        self.hints.push(line(args.iter().map(String::as_str)));
        self
    }

    /// `duscope <verb> <path> <flags> --scan <id> --json`. A path that is
    /// not UTF-8 (read lossily) names no file, and gets no hint.
    pub fn run(&mut self, verb: &str, path: Option<&str>, flags: &[&str]) -> &mut Hints {
        let Some(path) = path.filter(|p| !p.contains('\u{fffd}')) else {
            return self;
        };
        let mut args = vec![verb, path];
        args.extend(flags);
        args.extend(["--scan", &self.scan, "--json"]);
        self.hints.push(line(args));
        self
    }

    /// `duscope explain --errors <id> --json`.
    pub fn errors(&mut self) -> &mut Hints {
        let line = line(["explain", "--errors", &self.scan, "--json"]);
        self.hints.push(line);
        self
    }

    /// The hints, the first three.
    pub fn done(&mut self) -> Vec<String> {
        let mut hints = std::mem::take(&mut self.hints);
        hints.truncate(MAX);
        hints
    }
}

/// The hints of a list verb's envelope, by its verb.
pub fn list<T: Hinted>(e: &Envelope<T>) -> Vec<String> {
    let Some(mut hints) = Hints::new(e.scan.id.as_deref()) else {
        return Vec::new();
    };
    let path = Some(e.query.path.as_str());
    let first = e.entries.iter().find_map(Hinted::hint_path);
    hints.page(e.next_cursor.as_deref());
    match e.query.command {
        "scan" => hints
            .run("top", path, &[])
            .run("by-type", path, &[])
            .run("caches", path, &[]),
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
        line.push_str(&quote(arg));
    }
    line
}

/// `arg` as a shell reads it back: as it is when every character is safe,
/// else in single quotes.
fn quote(arg: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        Cow::Borrowed(arg)
    } else {
        Cow::Owned(format!("'{}'", arg.replace('\'', r"'\''")))
    }
}

#[cfg(test)]
mod tests {
    use super::{Hints, line, quote};

    #[test]
    fn p2_cli_01_a_hint_is_a_runnable_command_line() {
        assert_eq!(quote("/home/a/b.txt"), "/home/a/b.txt");
        assert_eq!(quote("/home/my dir"), "'/home/my dir'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("$HOME"), "'$HOME'");
        assert_eq!(line(["top", "/a b"]), "duscope top '/a b'");
        let mut hints = Hints::new(Some("3f9a0c1d2e3f")).unwrap();
        let got = hints
            .run("explain", Some("/r/x"), &[])
            .run("top", Some("/r"), &["--files"])
            .run("explain", Some("/r/\u{fffd}"), &[])
            .run("caches", Some("/r"), &[])
            .run("stale", Some("/r"), &[])
            .done();
        assert_eq!(
            got,
            [
                "duscope explain /r/x --scan 3f9a0c1d2e3f --json",
                "duscope top /r --files --scan 3f9a0c1d2e3f --json",
                "duscope caches /r --scan 3f9a0c1d2e3f --json",
            ]
        );
        assert!(Hints::new(None).is_none());
    }
}
