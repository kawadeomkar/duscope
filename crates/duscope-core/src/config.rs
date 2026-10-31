//! The configuration file, `config.toml`.
//!
//! It is `$XDG_CONFIG_HOME/duscope/config.toml`, or
//! `~/.config/duscope/config.toml` when that is unset (on macOS too), or the
//! file `--config` or `DUSCOPE_CONFIG` names; `none` in either reads no
//! file. `[defaults]` holds flag defaults under the flags' long names,
//! `[excludes]` patterns every scan leaves out, and `[classes]` and
//! `[caches]` extra rule tables. The binary applies them below the flags and
//! the environment: flag > environment > file > built-in.
//!
//! There is no per-tree configuration file: one found inside a scanned tree
//! would be configuration read from untrusted data.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use crate::CoreError;
use crate::rules::line_of;

/// The value of `--config` or `DUSCOPE_CONFIG` that reads no file.
pub const NONE: &str = "none";

/// Where the configuration comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// `none`: the built-in defaults alone.
    None,
    /// The default path, which need not exist.
    Default(PathBuf),
    /// A file `--config` or `DUSCOPE_CONFIG` names, which must exist.
    Named(PathBuf),
}

impl ConfigSource {
    /// `--config` first, then `DUSCOPE_CONFIG`, then the default path under
    /// `XDG_CONFIG_HOME` (when absolute, as the XDG specification asks) or
    /// `HOME`. `env` reads a variable; the binary passes `std::env::var_os`.
    pub fn locate(flag: Option<&OsStr>, env: impl Fn(&str) -> Option<OsString>) -> ConfigSource {
        let named = flag
            .map(OsStr::to_owned)
            .or_else(|| env("DUSCOPE_CONFIG").filter(|v| !v.is_empty()));
        if let Some(named) = named {
            return if named == NONE {
                ConfigSource::None
            } else {
                ConfigSource::Named(PathBuf::from(named))
            };
        }
        let set = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let base = set("XDG_CONFIG_HOME")
            .filter(|p| p.is_absolute())
            .or_else(|| set("HOME").map(|home| home.join(".config")));
        match base {
            Some(base) => ConfigSource::Default(base.join("duscope").join("config.toml")),
            None => ConfigSource::None,
        }
    }
}

/// A value from the file, with the line it is on, so that a value the
/// binary refuses can be reported where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting<T> {
    /// The value.
    pub value: T,
    /// The 1-based line of its key.
    pub line: usize,
}

/// `[defaults]`: the defaults of the flags of the same names. The binary
/// checks the values of the enumerated ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Defaults {
    /// `--threads`.
    pub threads: Option<Setting<u64>>,
    /// `--theme`: a pack's name, or a theme file's path (relative to the
    /// configuration file's directory).
    pub theme: Option<Setting<String>>,
    /// `--color-by`.
    pub color_by: Option<Setting<String>>,
    /// `--format`.
    pub format: Option<Setting<String>>,
    /// `--limit`.
    pub limit: Option<Setting<u64>>,
    /// `--size`.
    pub size: Option<Setting<String>>,
    /// `--attrs`.
    pub attrs: Option<Setting<String>>,
    /// `--clones`, or `--no-clones` when false.
    pub clones: Option<Setting<bool>>,
    /// `--max-age`, which arrives with the index reader.
    pub max_age: Option<Setting<String>>,
    /// `--index-dir` (relative to the configuration file's directory), which
    /// arrives with the index writer.
    pub index_dir: Option<Setting<PathBuf>>,
}

/// The configuration: what the file says, or nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The file it was read from, when one was.
    pub file: Option<PathBuf>,
    /// `[defaults]`.
    pub defaults: Defaults,
    /// `[excludes] patterns`: `--exclude` patterns every scan applies.
    pub excludes: Vec<String>,
    /// `[classes] extra`: a classes table merged over the vendored one
    /// (relative to the configuration file's directory).
    pub classes_extra: Option<PathBuf>,
    /// `[caches] extra`: a caches table merged over the vendored one.
    pub caches_extra: Option<PathBuf>,
}

/// The file as written.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    defaults: DefaultsFile,
    #[serde(default)]
    excludes: ExcludesFile,
    #[serde(default)]
    classes: ExtraFile,
    #[serde(default)]
    caches: ExtraFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultsFile {
    threads: Option<Spanned<u64>>,
    theme: Option<Spanned<String>>,
    color_by: Option<Spanned<String>>,
    format: Option<Spanned<String>>,
    limit: Option<Spanned<u64>>,
    size: Option<Spanned<String>>,
    attrs: Option<Spanned<String>>,
    clones: Option<Spanned<bool>>,
    max_age: Option<Spanned<String>>,
    index_dir: Option<Spanned<PathBuf>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExcludesFile {
    #[serde(default)]
    patterns: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtraFile {
    extra: Option<PathBuf>,
}

impl Config {
    /// Reads the configuration `source` names. A missing default file is
    /// the built-in defaults; a missing named file is an error.
    pub fn load(source: &ConfigSource) -> Result<Config, CoreError> {
        let (path, required) = match source {
            ConfigSource::None => return Ok(Config::default()),
            ConfigSource::Default(path) => (path, false),
            ConfigSource::Named(path) => (path, true),
        };
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text, path),
            Err(e) if !required && e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(CoreError::ConfigRead {
                file: path.display().to_string(),
                source: e,
            }),
        }
    }

    /// Parses the text of the file at `path`; relative paths in it are
    /// taken from the file's directory.
    pub fn parse(text: &str, path: &Path) -> Result<Config, CoreError> {
        let file = path.display().to_string();
        let parsed: ConfigFile = toml::from_str(text).map_err(|e| CoreError::Config {
            file: file.clone(),
            line: e.span().map_or(1, |span| line_of(text, &span)),
            message: e.message().to_owned(),
        })?;
        let dir = path.parent().unwrap_or(Path::new(""));
        let relative = |p: PathBuf| if p.is_absolute() { p } else { dir.join(p) };
        let d = parsed.defaults;
        let defaults = Defaults {
            threads: d.threads.map(|s| setting(text, s)),
            // A name stays a name; a path (it has a separator) is relative to
            // the file.
            theme: d.theme.map(|s| {
                let mut theme = setting(text, s);
                if theme.value.contains('/') {
                    theme.value = relative(PathBuf::from(&theme.value)).display().to_string();
                }
                theme
            }),
            color_by: d.color_by.map(|s| setting(text, s)),
            format: d.format.map(|s| setting(text, s)),
            limit: d.limit.map(|s| setting(text, s)),
            size: d.size.map(|s| setting(text, s)),
            attrs: d.attrs.map(|s| setting(text, s)),
            clones: d.clones.map(|s| setting(text, s)),
            max_age: d.max_age.map(|s| setting(text, s)),
            index_dir: d.index_dir.map(|s| {
                let mut dir = setting(text, s);
                dir.value = relative(dir.value);
                dir
            }),
        };
        Ok(Config {
            file: Some(path.to_owned()),
            defaults,
            excludes: parsed.excludes.patterns,
            classes_extra: parsed.classes.extra.map(relative),
            caches_extra: parsed.caches.extra.map(relative),
        })
    }

    /// Where a value on `line` is, for a message: `file:line`.
    pub fn at(&self, line: usize) -> String {
        let file = self
            .file
            .as_deref()
            .map_or_else(|| "config.toml".to_owned(), |f| f.display().to_string());
        format!("{file}:{line}")
    }
}

/// A value of the file with the line its key is on.
fn setting<T>(text: &str, spanned: Spanned<T>) -> Setting<T> {
    let line = line_of(text, &spanned.span());
    Setting {
        value: spanned.into_inner(),
        line,
    }
}

#[cfg(test)]
mod tests;
