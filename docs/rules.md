# Rule tables and theme files

duscope reads three kinds of TOML data file:
- `classes.toml` gives the file classes that `by-type` groups by and the treemap colours with.
- `caches.toml` gives the cache rules that `caches` reports and `scan --exclude-caches` does not enter.
- A user theme file recolours the treemap.

The vendored tables are in `crates/duscope-core/src/rules/` and are the canonical examples. They are reviewed like code, so a rule change is a pull request of its own. A user table is merged over the vendored one:
- `by-type --classes FILE` merges a classes table;
- `[classes] extra = "<path>"` and `[caches] extra = "<path>"` in the config file merge tables for every command.

An error in a table names its file and line, as in `rules.toml:3: unknown variant "snakes"`.

## classes.toml

There are seven classes: `code`, `media`, `archives`, `data`, `binaries`, `docs` and `other`. A file whose extension no rule lists is `other`.

| Table | Key | Value |
| --- | --- | --- |
| `[ext]` | an extension, as the ext rule below produces it | a class |
| `[dir]` | a directory name or glob, or several joined by `/` that must match the directory's last path components | a class |

**The ext rule.** A name's extension is its last suffix, lower-cased, at most 16 bytes of `[A-Za-z0-9+~-]`. The five suffixes `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst` and `tar.lz4` are kept whole. A dotfile (`.bashrc`), an undotted name and a trailing dot have no extension. A key that the rule could never produce, such as `"tar.foo.bar"`, is an error.

**Directory rules** win over the extension of every file beneath the directory. When rules overlap, a user table's rules are tried before the vendored ones. In a user table, an `[ext]` key replaces the vendored key of the same name.

```toml classes
[ext]
"tar.gz" = "archives"
rs = "code"
heic = "media"

[dir]
node_modules = "code"
".git/objects" = "data"
"*.app/Contents/MacOS" = "binaries"
```

## caches.toml

Each `[[rule]]` names a directory and what confirms that it is a cache:

| Field | Meaning |
| --- | --- |
| `name` | the directory's name, or a glob over it (one path component) |
| `markers` | file names, any one of which in an ancestor of the directory confirms it; at scan time the scanner looks only in the directory's parent |
| `under` | a path that the directory's parent must end with (`Library/Developer/Xcode`) |
| `cachedir_tag` | `true`: any directory holding a valid `CACHEDIR.TAG`, whatever its name; the rule takes no `name`, `markers` or `under` |
| `regenerable_by` | what rebuilds it, from a fixed set |

A named rule takes either `markers` or `under`. A name without its marker is not reported (precision over recall) unless `--loose` is given.

`regenerable_by` is one of `npm install`, `cargo build`, `pip install`, `gradle build`, `cmake --build`, `pod install`, `composer install`, `xcodebuild` or `unknown`. It is never free text, and anything else is an error.

A user table's rules are tried before the vendored ones, so a user rule can claim a name for another marker.

```toml caches
[[rule]]
name = "node_modules"
markers = ["package.json"]
regenerable_by = "npm install"

[[rule]]
name = "DerivedData"
under = "Library/Developer/Xcode"
regenerable_by = "xcodebuild"

[[rule]]
cachedir_tag = true
regenerable_by = "unknown"
```

**At scan time.** `scan --exclude-caches` passes each named rule to the scanner as a name and its markers. A rule confirmed by `under` passes its name alone. The `CACHEDIR.TAG` rule is the scanner's own check: it reads the first 43 bytes of the tag.

## User theme files

`--theme PATH` (or `DUSCOPE_THEME=PATH`) loads a theme from a TOML file. Every colour is `#rrggbb`. The file has four parts:

| Part | Contents |
| --- | --- |
| `[roles]` | the thirteen roles: `base`, `mantle`, `crust`, `surface1`, `surface2`, `overlay1`, `subtext`, `text`, `accent`, `title`, `link`, `red` (warnings), `green` (success) |
| `[classes]` | one colour for each of the seven classes |
| `cycle` | ten colours, for `--color-by folder` |
| `age` | five colours, newest to oldest, for `--color-by age` |

The colours drawn as borders or fills (`link`, `overlay1`, `surface1`, `surface2`, `accent`) must differ from the seven class colours. This example is Catppuccin Mocha, the default:

```toml theme
cycle = ["#89b4fa", "#f5c2e7", "#fab387", "#94e2d5", "#cba6f7", "#a6e3a1", "#f9e2af", "#eba0ac", "#74c7ec", "#b4befe"]
age = ["#a6e3a1", "#94e2d5", "#f9e2af", "#fab387", "#f38ba8"]

[roles]
base = "#1e1e2e"
mantle = "#181825"
crust = "#11111b"
surface1 = "#45475a"
surface2 = "#585b70"
overlay1 = "#7f849c"
subtext = "#a6adc8"
text = "#cdd6f4"
accent = "#f9e2af"
title = "#b4befe"
link = "#74c7ec"
red = "#f38ba8"
green = "#a6e3a1"

[classes]
code = "#89b4fa"
media = "#f5c2e7"
archives = "#fab387"
data = "#94e2d5"
binaries = "#cba6f7"
docs = "#a6e3a1"
other = "#6c7086"
```
