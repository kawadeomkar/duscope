//! P5-SEC-01: the external-input audit. Every call that reaches the
//! filesystem by a path or a name, in the code the binary is built from, is
//! on `fs_allowlist.txt` beside this file: its file, the function it is in
//! and the call, with what reaches it, from whom, and why that is safe. A
//! call the list does not hold fails the test until it is reviewed and
//! listed, and so does a listed call that is gone, so the list stays the
//! code's.
//!
//! The calls:
//! - `std::fs`'s functions by any path to them (`std::fs::read`, or
//!   `fs::read` after `use std::fs`), and `std::os::unix::fs`'s (`symlink`,
//!   `chown`, `fchown`, `lchown`, `chroot`, `mkfifo`); a `use` that imports
//!   one by its name is listed too, as its calls then go by that name alone;
//! - the constructors that open or make an entry by its path: `File::open`,
//!   `create`, `create_new` and `options`, `OpenOptions::new` and
//!   `DirBuilder::new`;
//! - the methods of `Path` and `DirEntry` that call into `std::fs`, each
//!   called with no argument: `canonicalize`, `read_dir`, `read_link`,
//!   `metadata`, `symlink_metadata`, `exists` and `try_exists` (a method of
//!   the same name that takes one, as `Provider::read_dir` does, is
//!   another's);
//! - the opens outside `std` that take a path or a name: rusqlite's
//!   `Connection::open` and `open_with_flags`, and the libc calls that open,
//!   make, rename or remove an entry (`open`, `openat`, `mkdirat`,
//!   `unlinkat`, `renameat`, and the `renameat2` and `openat2` system calls),
//!   which only `duscope-walk`'s platform modules make.
//!
//! Left out: `Path::is_file`, `is_dir` and `is_symlink`, a `stat` each, are
//! named as `Metadata`'s and `FileType`'s methods are, which read nothing, so
//! the text cannot tell them apart; calls on a descriptor (`File::from`,
//! `read`, `getdents64`), which reach only what a listed call opened; and the
//! scanner's own metadata reads through libc (`fstatat`, `statx`,
//! `getattrlistbulk` and the like), which read what a scan exists to read and
//! open nothing.
//!
//! The code read: each crate of the workspace the binary links (its normal
//! dependencies, followed from its manifest), from the crate's root module
//! through the modules its `mod` items declare, macOS's and Linux's alike
//! whatever this test runs on, without what `#[cfg(test)]` marks (a test
//! module, a function or a field kept for tests) or a module only such an
//! item declares. Comments and literals are not code. Not read:
//! `duscope-testkit` (tests only), the fuzz targets, `duscope-walk`'s
//! examples and `tests/fixtures/mktree`, none of which the binary holds.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use cargo_metadata::{DependencyKind, MetadataCommand};

/// `std::os::unix::fs`'s functions.
const UNIX_FS: [&str; 6] = ["symlink", "chown", "fchown", "lchown", "chroot", "mkfifo"];
/// The constructors of `std::fs`'s types that open or make by path.
const OPENERS: [(&str, &[&str]); 3] = [
    ("File", &["open", "create", "create_new", "options"]),
    ("OpenOptions", &["new"]),
    ("DirBuilder", &["new"]),
];
/// The methods that call into `std::fs`.
const METHODS: [&str; 7] = [
    "canonicalize",
    "read_dir",
    "read_link",
    "metadata",
    "symlink_metadata",
    "exists",
    "try_exists",
];
/// The libc calls that open, make, rename or remove an entry by its name.
const LIBC: [&str; 5] = ["open", "openat", "mkdirat", "unlinkat", "renameat"];
/// The system calls of the same kind libc has no wrapper for.
const SYSCALLS: [&str; 2] = ["SYS_renameat2", "SYS_openat2"];

/// A token of Rust source, as much as the audit needs: literals and
/// lifetimes stand for themselves, comments are gone.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    /// `::`
    PathSep,
    Punct(char),
    Lit,
}

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The index after a quoted literal whose opening quote is before `i`.
fn after_quoted(s: &[char], mut i: usize, quote: char) -> usize {
    while i < s.len() {
        match s[i] {
            '\\' => i += 2,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    i
}

/// The index after a raw string whose `#`s, if any, start at `i`.
fn after_raw(s: &[char], mut i: usize) -> usize {
    let hashes = s[i..].iter().take_while(|&&c| c == '#').count();
    i += hashes + 1;
    while i < s.len() {
        if s[i] == '"'
            && s[i + 1..]
                .iter()
                .take(hashes)
                .filter(|&&c| c == '#')
                .count()
                == hashes
        {
            return i + 1 + hashes;
        }
        i += 1;
    }
    i
}

/// The index after a comment that starts at `i` (`//` or a nested `/*`).
fn after_comment(s: &[char], mut i: usize) -> usize {
    if s[i + 1] == '/' {
        while i < s.len() && s[i] != '\n' {
            i += 1;
        }
        return i;
    }
    let mut depth = 0;
    while i < s.len() {
        if s[i] == '/' && s.get(i + 1) == Some(&'*') {
            depth += 1;
            i += 2;
        } else if s[i] == '*' && s.get(i + 1) == Some(&'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                break;
            }
        } else {
            i += 1;
        }
    }
    i
}

/// A word that opens a literal (`r"…"`, `b"…"`, `b'…'`, `c"…"`): the index
/// after the literal, or `None` where the word is a word.
fn after_prefixed(s: &[char], word: &str, i: usize) -> Option<usize> {
    let at = |k: usize| s.get(k).copied().unwrap_or('\0');
    match word {
        "r" | "br" | "cr" if at(i) == '"' || (at(i) == '#' && matches!(at(i + 1), '"' | '#')) => {
            Some(after_raw(s, i))
        }
        "b" | "c" if at(i) == '"' => Some(after_quoted(s, i + 1, '"')),
        "b" if at(i) == '\'' => Some(after_quoted(s, i + 1, '\'')),
        _ => None,
    }
}

fn lex(src: &str) -> Vec<Tok> {
    let s: Vec<char> = src.chars().collect();
    let at = |k: usize| s.get(k).copied().unwrap_or('\0');
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && matches!(at(i + 1), '/' | '*') {
            i = after_comment(&s, i);
        } else if c == '"' {
            i = after_quoted(&s, i + 1, '"');
            out.push(Tok::Lit);
        } else if c == '\'' {
            if at(i + 1) == '\\' || at(i + 2) == '\'' {
                // A character: `'x'`, `'\n'`, `'\''`, `'\u{1F600}'`.
                i = after_quoted(&s, i + 1, '\'');
                out.push(Tok::Lit);
            } else {
                // A lifetime or a label.
                i += 1;
                while ident_char(at(i)) {
                    i += 1;
                }
            }
        } else if ident_start(c) {
            let start = i;
            while ident_char(at(i)) {
                i += 1;
            }
            let word: String = s[start..i].iter().collect();
            if let Some(after) = after_prefixed(&s, &word, i) {
                i = after;
                out.push(Tok::Lit);
            } else if word == "r" && at(i) == '#' {
                // A raw identifier, `r#type`.
                let start = i + 1;
                i = start;
                while ident_char(at(i)) {
                    i += 1;
                }
                out.push(Tok::Ident(s[start..i].iter().collect()));
            } else {
                out.push(Tok::Ident(word));
            }
        } else if c.is_ascii_digit() {
            while ident_char(at(i)) || (at(i) == '.' && at(i + 1).is_ascii_digit()) {
                i += 1;
            }
            out.push(Tok::Lit);
        } else if c == ':' && at(i + 1) == ':' {
            out.push(Tok::PathSep);
            i += 2;
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

fn is(tok: Option<&Tok>, c: char) -> bool {
    tok == Some(&Tok::Punct(c))
}

fn word(tok: Option<&Tok>) -> Option<&str> {
    match tok {
        Some(Tok::Ident(w)) => Some(w),
        _ => None,
    }
}

/// The index after the bracket that closes the one at `i`.
fn after_group(toks: &[Tok], mut i: usize) -> usize {
    let mut depth = 0usize;
    while i < toks.len() {
        match toks[i] {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Whether an attribute's tokens (inside `#[…]`) are `cfg(test)` or
/// `cfg(all(…, test, …))`: code built for tests alone.
fn test_only(attr: &[Tok]) -> bool {
    if word(attr.first()) != Some("cfg") || !is(attr.get(1), '(') {
        return false;
    }
    match word(attr.get(2)) {
        Some("test") => is(attr.get(3), ')'),
        Some("all") if is(attr.get(3), '(') => {
            let mut depth = 0usize;
            for (k, tok) in attr.iter().enumerate().skip(3) {
                match tok {
                    Tok::Punct('(') => depth += 1,
                    Tok::Punct(')') => depth -= 1,
                    Tok::Ident(w) if w == "test" && depth == 1 => {
                        let before = attr.get(k - 1);
                        let after = attr.get(k + 1);
                        if (is(before, '(') || is(before, ','))
                            && (is(after, ',') || is(after, ')'))
                        {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        _ => false,
    }
}

/// The index after the item, statement, field or expression that starts
/// at `i`, after its attributes: one `#[cfg(test)]` marks.
fn after_item(toks: &[Tok], mut i: usize) -> usize {
    while is(toks.get(i), '#') && is(toks.get(i + 1), '[') {
        i = after_group(toks, i + 1);
    }
    let mut k = i;
    if word(toks.get(k)) == Some("pub") {
        k += 1;
        if is(toks.get(k), '(') {
            k = after_group(toks, k);
        }
    }
    while matches!(
        word(toks.get(k)),
        Some("async" | "unsafe" | "default" | "extern")
    ) || (word(toks.get(k)) == Some("const") && word(toks.get(k + 1)) == Some("fn"))
    {
        k += 1;
        if toks.get(k) == Some(&Tok::Lit) {
            k += 1;
        }
    }
    // An item ends at its `;` or its block, whatever commas its generics
    // or `where` clause hold; anything else at a comma too (a field).
    let item = matches!(
        word(toks.get(k)),
        Some(
            "fn" | "impl"
                | "mod"
                | "struct"
                | "enum"
                | "trait"
                | "union"
                | "type"
                | "use"
                | "static"
                | "const"
                | "macro_rules"
        )
    );
    let mut depth = 0usize;
    while i < toks.len() {
        match toks[i] {
            Tok::Punct('{') if depth == 0 => return after_group(toks, i),
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            Tok::Punct(';') if depth == 0 => return i + 1,
            Tok::Punct(',') if depth == 0 && !item => return i + 1,
            _ => {}
        }
        i += 1;
    }
    i
}

/// The call a path names, if it is one the audit lists: `segments` are its
/// words, `next` the token after it.
fn path_call(segments: &[&str], next: Option<&Tok>) -> Option<String> {
    let rest = match segments.first() {
        Some(&"std") => &segments[1..],
        _ => segments,
    };
    let unix = match rest {
        ["os", "unix", "fs", f, ..] | ["unix", "fs", f, ..] => Some(*f),
        _ => None,
    };
    if let Some(f) = unix {
        return UNIX_FS.contains(&f).then(|| format!("unix::fs::{f}"));
    }
    let typed = match rest {
        ["fs", f, ..] if f.starts_with(char::is_lowercase) => return Some(format!("fs::{f}")),
        ["fs", ty, method, ..] | [ty, method, ..] => Some((*ty, *method)),
        _ => None,
    };
    if let Some((ty, method)) = typed
        && OPENERS
            .iter()
            .any(|(t, methods)| *t == ty && methods.contains(&method))
    {
        return Some(format!("{ty}::{method}"));
    }
    match segments {
        ["libc", f] if LIBC.contains(f) && is(next, '(') => Some(format!("libc::{f}")),
        ["libc", f] if SYSCALLS.contains(f) => Some(format!("libc::{f}")),
        [.., "Connection", f]
            if f.starts_with("open") && !f.starts_with("open_in_memory") && is(next, '(') =>
        {
            Some(format!("Connection::{f}"))
        }
        _ => None,
    }
}

/// The functions a `use` statement's tokens (after `use`) import by name
/// from `std::fs`, `std::os::unix::fs` or libc, as calls.
fn imported_calls(toks: &[Tok]) -> Vec<String> {
    let mut calls = Vec::new();
    // A name imported as it is, not a module on the way to one.
    let leaf = |k: usize| toks.get(k + 1) != Some(&Tok::PathSep);
    match toks.iter().find_map(|t| word(Some(t))) {
        Some("std") => {
            for k in 0..toks.len() {
                if word(toks.get(k)) != Some("fs") || toks.get(k + 1) != Some(&Tok::PathSep) {
                    continue;
                }
                let module = if k >= 2 && word(toks.get(k - 2)) == Some("unix") {
                    "unix::fs"
                } else {
                    "fs"
                };
                let mut names = Vec::new();
                if is(toks.get(k + 2), '{') {
                    let mut depth = 0usize;
                    for j in k + 2..after_group(toks, k + 2) {
                        match &toks[j] {
                            Tok::Punct('{') => depth += 1,
                            Tok::Punct('}') => depth -= 1,
                            Tok::Ident(name)
                                if depth == 1
                                    && leaf(j)
                                    && (is(toks.get(j - 1), '{') || is(toks.get(j - 1), ',')) =>
                            {
                                names.push(name.as_str());
                            }
                            _ => {}
                        }
                    }
                } else if let Some(name) = word(toks.get(k + 2))
                    && leaf(k + 2)
                {
                    names.push(name);
                }
                for name in names {
                    if name != "self" && name.starts_with(char::is_lowercase) {
                        calls.push(format!("use {module}::{name}"));
                    }
                }
            }
        }
        Some("libc") => {
            for tok in toks {
                if let Tok::Ident(name) = tok
                    && LIBC.contains(&name.as_str())
                {
                    calls.push(format!("use libc::{name}"));
                }
            }
        }
        _ => {}
    }
    calls
}

/// Whether the word at `i` starts a path: no `::` before it, or a leading
/// one (`::std::fs`).
fn path_starts(toks: &[Tok], i: usize) -> bool {
    i == 0
        || toks.get(i - 1) != Some(&Tok::PathSep)
        || (i >= 2 && !matches!(toks.get(i - 2), Some(Tok::Ident(_) | Tok::Punct('>'))))
}

/// What one file holds for the audit.
#[derive(Debug, Default, PartialEq)]
struct FileScan {
    /// The calls, each with the function it is in (`(item)` outside one).
    calls: Vec<(String, String)>,
    /// The modules declared out of line, with the inline modules around each.
    mods: Vec<(Vec<String>, String)>,
    /// `#![cfg(test)]`: the whole file is for tests.
    test_only: bool,
}

/// What `scan` keeps track of: the brace depth, the bracket depth, the
/// functions and inline modules open, and a function whose body is next.
#[derive(Default)]
struct Walker {
    braces: usize,
    brackets: usize,
    fns: Vec<(String, usize)>,
    inline: Vec<(String, usize)>,
    next_fn: Option<(String, usize)>,
    next_mod: Option<String>,
}

impl Walker {
    fn func(&self) -> String {
        self.fns
            .last()
            .map_or_else(|| "(item)".to_owned(), |(name, _)| name.clone())
    }

    fn open(&mut self, c: char) {
        if c != '{' {
            self.brackets += 1;
            return;
        }
        self.braces += 1;
        if let Some((name, brackets)) = self.next_fn.take() {
            if brackets == self.brackets {
                self.fns.push((name, self.braces));
            } else {
                self.next_fn = Some((name, brackets));
            }
        }
        if let Some(name) = self.next_mod.take() {
            self.inline.push((name, self.braces));
        }
    }

    fn close(&mut self, c: char) {
        if c != '}' {
            self.brackets = self.brackets.saturating_sub(1);
            return;
        }
        if self.fns.last().is_some_and(|(_, d)| *d == self.braces) {
            self.fns.pop();
        }
        if self.inline.last().is_some_and(|(_, d)| *d == self.braces) {
            self.inline.pop();
        }
        self.braces = self.braces.saturating_sub(1);
    }
}

/// Reads one file's tokens: its calls, its `mod` declarations, and whether
/// it is for tests alone.
fn scan(toks: &[Tok]) -> FileScan {
    let mut found = FileScan::default();
    let mut w = Walker::default();
    let mut i = 0;
    while i < toks.len() {
        let next = toks.get(i + 1);
        match &toks[i] {
            Tok::Punct('#') if is(next, '[') || (is(next, '!') && is(toks.get(i + 2), '[')) => {
                let open = if is(next, '!') { i + 2 } else { i + 1 };
                let end = after_group(toks, open);
                let attr = &toks[open + 1..end - 1];
                assert!(
                    word(attr.first()) != Some("path"),
                    "a #[path] attribute: the audit finds modules by their names"
                );
                if test_only(attr) {
                    if open == i + 2 {
                        found.test_only = true;
                        return found;
                    }
                    i = after_item(toks, end);
                } else {
                    i = end;
                }
                continue;
            }
            Tok::Ident(kw) if kw == "use" && !is(next, '<') => {
                let end = (i..toks.len())
                    .find(|&k| is(toks.get(k), ';'))
                    .unwrap_or(toks.len());
                for call in imported_calls(&toks[i + 1..end]) {
                    found.calls.push((w.func(), call));
                }
                i = end + 1;
                continue;
            }
            Tok::Ident(kw) if kw == "mod" => {
                if let Some(name) = word(next) {
                    if is(toks.get(i + 2), ';') {
                        let inline = w.inline.iter().map(|(n, _)| n.clone()).collect();
                        found.mods.push((inline, name.to_owned()));
                    } else {
                        w.next_mod = Some(name.to_owned());
                    }
                }
            }
            Tok::Ident(kw) if kw == "fn" => {
                if let Some(name) = word(next) {
                    w.next_fn = Some((name.to_owned(), w.brackets));
                }
            }
            Tok::Ident(first) if next == Some(&Tok::PathSep) && path_starts(toks, i) => {
                let mut segments = vec![first.as_str()];
                let mut j = i;
                while toks.get(j + 1) == Some(&Tok::PathSep) {
                    let Some(seg) = word(toks.get(j + 2)) else {
                        break;
                    };
                    segments.push(seg);
                    j += 2;
                }
                if let Some(call) = path_call(&segments, toks.get(j + 1)) {
                    found.calls.push((w.func(), call));
                }
                i = j + 1;
                continue;
            }
            Tok::Punct('.') => {
                // Each takes no argument: a method of the same name that
                // takes one (`Provider::read_dir`) is another's.
                if let Some(m) = word(next)
                    && METHODS.contains(&m)
                    && is(toks.get(i + 2), '(')
                    && is(toks.get(i + 3), ')')
                {
                    found.calls.push((w.func(), format!(".{m}()")));
                }
            }
            Tok::Punct(c @ ('{' | '(' | '[')) => w.open(*c),
            Tok::Punct(c @ ('}' | ')' | ']')) => w.close(*c),
            Tok::Punct(';') => {
                if w.next_fn.as_ref().is_some_and(|(_, b)| *b == w.brackets) {
                    w.next_fn = None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    found
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap()
}

/// The root module of each crate of the workspace the binary links,
/// itself included.
fn crate_roots() -> Vec<PathBuf> {
    let metadata = MetadataCommand::new()
        .manifest_path(repo().join("Cargo.toml"))
        .no_deps()
        .exec()
        .unwrap();
    let members = metadata.workspace_packages();
    let mut names = vec!["duscope".to_owned()];
    let mut roots = Vec::new();
    let mut k = 0;
    while k < names.len() {
        let package = members
            .iter()
            .find(|p| p.name.as_str() == names[k])
            .unwrap();
        for dep in &package.dependencies {
            if dep.kind == DependencyKind::Normal
                && dep.path.is_some()
                && !names.contains(&dep.name)
            {
                names.push(dep.name.clone());
            }
        }
        for target in &package.targets {
            if target.is_lib() || target.is_bin() {
                roots.push(target.src_path.clone().into_std_path_buf());
            }
        }
        k += 1;
    }
    roots
}

/// The file of module `name` declared in `file` inside the inline modules
/// `inline`.
fn module_file(file: &Path, is_root: bool, inline: &[String], name: &str) -> PathBuf {
    let stem = file.file_stem().unwrap().to_str().unwrap();
    let mut dir = file.parent().unwrap().to_owned();
    if !is_root && stem != "mod" {
        dir.push(stem);
    }
    dir.extend(inline);
    let flat = dir.join(format!("{name}.rs"));
    let nested = dir.join(name).join("mod.rs");
    match (flat.is_file(), nested.is_file()) {
        (true, false) => flat,
        (false, true) => nested,
        _ => panic!("module {name} of {}: no single file for it", file.display()),
    }
}

/// Every call the audit lists, by file (relative to the repository) and
/// function, with how many times each is made there.
type Inventory = BTreeMap<(String, String), BTreeMap<String, usize>>;

fn inventory() -> Inventory {
    let repo = repo();
    let mut calls = Inventory::new();
    let mut todo: Vec<(PathBuf, bool)> = crate_roots().into_iter().map(|r| (r, true)).collect();
    let mut seen = BTreeSet::new();
    while let Some((file, is_root)) = todo.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let found = scan(&lex(&fs::read_to_string(&file).unwrap()));
        if found.test_only {
            continue;
        }
        let rel = file
            .strip_prefix(&repo)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        for (func, call) in found.calls {
            *calls
                .entry((rel.clone(), func))
                .or_default()
                .entry(call)
                .or_default() += 1;
        }
        for (inline, name) in found.mods {
            todo.push((module_file(&file, is_root, &inline, &name), false));
        }
    }
    calls
}

/// One entry of the list: the calls of one function, and why they are
/// safe.
#[derive(Debug)]
struct Entry {
    calls: BTreeMap<String, usize>,
    reason: String,
}

/// `fs_allowlist.txt`: a `## <file>` line, then for each function a line
/// `<function>: <call>[ xN][, <call>[ xN]]…` and the reason, indented, on
/// the lines after it. Lines that start with `#` and a space are comments.
fn allow_list(text: &str) -> BTreeMap<(String, String), Entry> {
    let mut list = BTreeMap::new();
    let mut file: Option<String> = None;
    let mut current: Option<(String, String)> = None;
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        if let Some(name) = line.strip_prefix("## ") {
            file = Some(name.trim().to_owned());
            current = None;
        } else if line.starts_with("# ") || line == "#" || line.trim().is_empty() {
        } else if line.starts_with(' ') {
            let key = current
                .as_ref()
                .unwrap_or_else(|| panic!("line {n}: a reason with no entry above it"));
            let entry: &mut Entry = list.get_mut(key).unwrap();
            if !entry.reason.is_empty() {
                entry.reason.push(' ');
            }
            entry.reason.push_str(line.trim());
        } else {
            let file = file
                .clone()
                .unwrap_or_else(|| panic!("line {n}: an entry before any `## <file>`"));
            let (func, list_of_calls) = line
                .split_once(": ")
                .unwrap_or_else(|| panic!("line {n}: not `<function>: <calls>`: {line}"));
            let mut calls = BTreeMap::new();
            for call in list_of_calls.split(", ") {
                let (call, count) = match call.rsplit_once(" x") {
                    Some((call, count)) if count.parse::<usize>().is_ok() => {
                        (call, count.parse().unwrap())
                    }
                    _ => (call, 1),
                };
                assert!(
                    calls.insert(call.to_owned(), count).is_none(),
                    "line {n}: {call} twice"
                );
            }
            let key = (file, func.to_owned());
            assert!(
                !list.contains_key(&key),
                "line {n}: a second entry for {func}"
            );
            list.insert(
                key.clone(),
                Entry {
                    calls,
                    reason: String::new(),
                },
            );
            current = Some(key);
        }
    }
    list
}

/// An entry's first line, as the list writes it.
fn entry_line(func: &str, calls: &BTreeMap<String, usize>) -> String {
    let calls: Vec<String> = calls
        .iter()
        .map(|(call, &n)| {
            if n == 1 {
                call.clone()
            } else {
                format!("{call} x{n}")
            }
        })
        .collect();
    format!("{func}: {}", calls.join(", "))
}

#[test]
fn p5_sec_01_every_fs_call_is_on_the_allow_list_with_its_reason() {
    let found = inventory();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fs_allowlist.txt");
    let list = allow_list(&fs::read_to_string(&path).unwrap());
    let mut unreviewed = String::new();
    let mut gone = String::new();
    let mut file = "";
    for ((f, func), calls) in &found {
        let listed = list.get(&(f.clone(), func.clone()));
        if listed.is_some_and(|entry| entry.calls == *calls) {
            continue;
        }
        if f != file {
            let _ = writeln!(unreviewed, "## {f}");
            file = f;
        }
        let _ = writeln!(unreviewed, "{}", entry_line(func, calls));
        let _ = writeln!(
            unreviewed,
            "  {}",
            listed.map_or("(new: what reaches it, from whom, why it is safe)", |e| {
                &e.reason
            })
        );
    }
    for ((f, func), entry) in &list {
        assert!(
            !entry.reason.is_empty(),
            "{f}: {func}: an entry without a reason"
        );
        if !found.contains_key(&(f.clone(), func.clone())) {
            let _ = writeln!(gone, "{f}: {}", entry_line(func, &entry.calls));
        }
    }
    assert!(
        unreviewed.is_empty() && gone.is_empty(),
        "fs_allowlist.txt is not the code's: review each call and write it there\n\
         calls the list does not hold, or holds a different number of:\n{unreviewed}\n\
         entries for calls that are gone:\n{gone}"
    );
}

#[test]
fn p5_sec_01_the_audit_reads_code_and_leaves_out_tests_comments_and_literals() {
    let src = r##"
        //! fs::read in a comment, /* and fs::write */
        use std::fs::{self, File};
        use std::os::unix::fs::{symlink, MetadataExt};
        mod real;
        #[cfg(test)]
        mod tests;
        #[cfg(target_os = "linux")]
        mod linux;
        const NOTE: &str = "std::fs::remove_file(path)";
        const RAW: &str = r#"fs::remove_dir_all("/")"#;
        const QUOTE: char = '"';
        fn reads<'a>(path: &'a Path) -> Vec<u8> {
            let _ = b'"';
            let _ = std::fs::metadata(path);
            let bytes = fs::read(path).unwrap_or_default();
            let opened = File::open(path).map(File::from);
            let _ = path.canonicalize();
            let _ = path.is_dir();
            bytes
        }
        #[cfg(test)]
        fn helper() {
            fs::write("x", b"y").unwrap();
        }
        #[cfg(not(test))]
        fn kept() {
            let make = || OpenOptions::new().create(true).open("x");
            let inner = {
                fn nested<T, U>(x: T) where T: Into<U>, U: Copy {
                    let _ = libc::openat(0, c"name".as_ptr(), 0);
                }
            };
            let _ = libc::syscall(libc::SYS_renameat2, 0);
            let db = rusqlite::Connection::open_with_flags(path, flags);
            let mem = Connection::open_in_memory();
        }
        struct S {
            #[cfg(test)]
            peak: usize,
            n: usize,
        }
        mod inner {
            mod deeper;
            pub fn read_link(p: &Path) { let _ = p.read_link(); }
        }
        #[cfg(all(unix, test))]
        mod unix_tests {
            fn f() { std::fs::remove_file("x"); }
        }
    "##;
    let found = scan(&lex(src));
    let calls: Vec<(&str, &str)> = found
        .calls
        .iter()
        .map(|(f, c)| (f.as_str(), c.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            ("(item)", "use unix::fs::symlink"),
            ("reads", "fs::metadata"),
            ("reads", "fs::read"),
            ("reads", "File::open"),
            ("reads", ".canonicalize()"),
            ("kept", "OpenOptions::new"),
            ("nested", "libc::openat"),
            ("kept", "libc::SYS_renameat2"),
            ("kept", "Connection::open_with_flags"),
            ("read_link", ".read_link()"),
        ]
    );
    assert_eq!(
        found.mods,
        [
            (vec![], "real".to_owned()),
            (vec![], "linux".to_owned()),
            (vec!["inner".to_owned()], "deeper".to_owned()),
        ]
    );
    assert!(!found.test_only);
    assert!(scan(&lex("#![cfg(test)]\nfn f() { std::fs::read(\"x\"); }")).test_only);
}
