//! The colour packs: Catppuccin Mocha (the default), Tokyo Night, Dracula,
//! Gruvbox Dark and a light pack from terminal.css, each thirteen role
//! colours, the seven class colours, a ten-colour cycle (`--color-by
//! folder` and `level`) and a five-step age ramp (`--color-by age`); the
//! `terminal-native` fallback in the 16 ANSI colours; and `no-color`, which
//! paints with `░▒▓█` shades. A user's TOML theme file (`docs/rules.md`)
//! makes an eighth.
//!
//! The roles drawn as borders or fills (`link`, `overlay1`, `surface1`,
//! `surface2`, `accent`) never share a colour with a class, so a tile and a
//! border never look alike ([`Pack::clashes`]).

use std::collections::BTreeMap;

use duscope_core::rules::Class;
use serde::Deserialize;

use crate::TreemapError;
use crate::cells::Color;

/// The roles every part of the UI draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roles {
    /// The screen background, pane bodies, title-tag background.
    pub base: Color,
    /// Window chrome.
    pub mantle: Color,
    /// Text on accent badges and on light tiles; gutter strips.
    pub crust: Color,
    /// Deep folder borders.
    pub surface1: Color,
    /// Depth-2 folder borders, bar tracks.
    pub surface2: Color,
    /// Depth-1 folder borders, muted labels.
    pub overlay1: Color,
    /// Secondary text, percentages.
    pub subtext: Color,
    /// Primary text.
    pub text: Color,
    /// Badges and key letters.
    pub accent: Color,
    /// Pane titles and the map's border.
    pub title: Color,
    /// The selection's ancestor folders.
    pub link: Color,
    /// Warnings.
    pub red: Color,
    /// Success.
    pub green: Color,
}

/// A colour pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    /// Its name (`mocha`, …, or the theme file's path).
    pub name: String,
    /// Whether it is meant for a dark background.
    pub dark: bool,
    /// The role colours.
    pub roles: Roles,
    /// The class colours, in [`Class::ALL`] order.
    pub classes: [Color; 7],
    /// Ten colours for `--color-by folder` and `level`.
    pub cycle: [Color; 10],
    /// Five colours for `--color-by age`, newest to oldest.
    pub age: [Color; 5],
    /// `no-color`: tiles are `░▒▓█` shades by depth, not fills.
    pub shades: bool,
}

/// The built-in packs' names, the default first.
pub const NAMES: [&str; 7] = [
    "mocha",
    "tokyo-night",
    "dracula",
    "gruvbox",
    "light",
    "terminal-native",
    "no-color",
];

fn rgb(hex: &str) -> Color {
    Color::hex(hex).unwrap_or(Color::Reset)
}

fn rgbs<const N: usize>(hexes: [&str; N]) -> [Color; N] {
    hexes.map(rgb)
}

#[allow(clippy::too_many_arguments)]
fn roles(
    base: &str,
    mantle: &str,
    crust: &str,
    surface1: &str,
    surface2: &str,
    overlay1: &str,
    subtext: &str,
    text: &str,
    accent: &str,
    title: &str,
    link: &str,
    red: &str,
    green: &str,
) -> Roles {
    Roles {
        base: rgb(base),
        mantle: rgb(mantle),
        crust: rgb(crust),
        surface1: rgb(surface1),
        surface2: rgb(surface2),
        overlay1: rgb(overlay1),
        subtext: rgb(subtext),
        text: rgb(text),
        accent: rgb(accent),
        title: rgb(title),
        link: rgb(link),
        red: rgb(red),
        green: rgb(green),
    }
}

impl Pack {
    /// Catppuccin Mocha, the default pack.
    pub fn mocha() -> Pack {
        Pack {
            name: "mocha".to_owned(),
            dark: true,
            roles: roles(
                "#1e1e2e", "#181825", "#11111b", "#45475a", "#585b70", "#7f849c", "#a6adc8",
                "#cdd6f4", "#f9e2af", "#b4befe", "#74c7ec", "#f38ba8", "#a6e3a1",
            ),
            classes: rgbs([
                "#89b4fa", "#f5c2e7", "#fab387", "#94e2d5", "#cba6f7", "#a6e3a1", "#6c7086",
            ]),
            cycle: rgbs([
                "#89b4fa", "#f5c2e7", "#fab387", "#94e2d5", "#cba6f7", "#a6e3a1", "#f9e2af",
                "#eba0ac", "#74c7ec", "#b4befe",
            ]),
            age: rgbs(["#a6e3a1", "#94e2d5", "#f9e2af", "#fab387", "#f38ba8"]),
            shades: false,
        }
    }

    /// A built-in pack by name; `catppuccin-mocha`, `gruvbox-dark` and
    /// `no-colour` are accepted too.
    // One literal table per pack.
    #[allow(clippy::too_many_lines)]
    pub fn named(name: &str) -> Option<Pack> {
        let name = match name {
            "catppuccin-mocha" | "catppuccin" => "mocha",
            "gruvbox-dark" => "gruvbox",
            "no-colour" => "no-color",
            other => other,
        };
        let pack = match name {
            "mocha" => Pack::mocha(),
            "tokyo-night" => Pack {
                name: name.to_owned(),
                dark: true,
                roles: roles(
                    "#1a1b26", "#16161e", "#0f0f14", "#292e42", "#3b4261", "#565f89", "#a9b1d6",
                    "#c0caf5", "#e0af68", "#bb9af7", "#7aa2f7", "#f7768e", "#9ece6a",
                ),
                classes: rgbs([
                    "#7dcfff", "#f7768e", "#ff9e64", "#73daca", "#bb9af7", "#9ece6a", "#737aa2",
                ]),
                cycle: rgbs([
                    "#7dcfff", "#f7768e", "#ff9e64", "#73daca", "#bb9af7", "#9ece6a", "#e0af68",
                    "#2ac3de", "#7aa2f7", "#ff007c",
                ]),
                age: rgbs(["#9ece6a", "#73daca", "#e0af68", "#ff9e64", "#f7768e"]),
                shades: false,
            },
            "dracula" => Pack {
                name: name.to_owned(),
                dark: true,
                roles: roles(
                    "#282a36", "#21222c", "#191a21", "#44475a", "#565a6e", "#7e8ab8", "#bfc7d5",
                    "#f8f8f2", "#f1fa8c", "#bd93f9", "#8be9fd", "#ff5555", "#50fa7b",
                ),
                classes: rgbs([
                    "#bd93f9", "#ff79c6", "#ffb86c", "#a4ffff", "#ff5555", "#50fa7b", "#6272a4",
                ]),
                cycle: rgbs([
                    "#bd93f9", "#ff79c6", "#ffb86c", "#8be9fd", "#50fa7b", "#ff5555", "#f1fa8c",
                    "#6272a4", "#ffd1e8", "#a4ffff",
                ]),
                age: rgbs(["#50fa7b", "#8be9fd", "#f1fa8c", "#ffb86c", "#ff5555"]),
                shades: false,
            },
            "gruvbox" => Pack {
                name: name.to_owned(),
                dark: true,
                roles: roles(
                    "#282828", "#1d2021", "#141617", "#504945", "#665c54", "#928374", "#bdae93",
                    "#ebdbb2", "#fabd2f", "#d3869b", "#83a598", "#fb4934", "#b8bb26",
                ),
                classes: rgbs([
                    "#458588", "#d3869b", "#fe8019", "#8ec07c", "#fb4934", "#b8bb26", "#7c6f64",
                ]),
                cycle: rgbs([
                    "#83a598", "#d3869b", "#fe8019", "#8ec07c", "#fb4934", "#b8bb26", "#fabd2f",
                    "#458588", "#b16286", "#689d6a",
                ]),
                age: rgbs(["#b8bb26", "#8ec07c", "#fabd2f", "#fe8019", "#fb4934"]),
                shades: false,
            },
            // terminal.css: #fff base, #151515 text, #727578 secondary,
            // #1a95e0 primary, #d20962 error.
            "light" => Pack {
                name: name.to_owned(),
                dark: false,
                roles: roles(
                    "#ffffff", "#f4f4f4", "#151515", "#e0e0e0", "#c8c8c8", "#9e9e9e", "#727578",
                    "#151515", "#1a95e0", "#151515", "#0d6aa8", "#d20962", "#2e7d32",
                ),
                classes: rgbs([
                    "#1565c0", "#c2185b", "#ef6c00", "#00897b", "#7b1fa2", "#2e7d32", "#8d8d8d",
                ]),
                cycle: rgbs([
                    "#1565c0", "#c2185b", "#ef6c00", "#00897b", "#7b1fa2", "#2e7d32", "#f9a825",
                    "#5e35b1", "#00acc1", "#6d4c41",
                ]),
                age: rgbs(["#2e7d32", "#00897b", "#f9a825", "#ef6c00", "#d20962"]),
                shades: false,
            },
            "terminal-native" => Pack {
                name: name.to_owned(),
                dark: true,
                roles: Roles {
                    base: Color::Reset,
                    mantle: Color::Reset,
                    crust: Color::Ansi(0),
                    surface1: Color::Ansi(8),
                    surface2: Color::Ansi(8),
                    overlay1: Color::Ansi(8),
                    subtext: Color::Ansi(7),
                    text: Color::Reset,
                    accent: Color::Ansi(11),
                    title: Color::Ansi(13),
                    link: Color::Ansi(12),
                    red: Color::Ansi(9),
                    green: Color::Ansi(10),
                },
                classes: [4, 5, 3, 6, 1, 2, 7].map(Color::Ansi),
                cycle: [4, 5, 3, 6, 1, 2, 14, 13, 10, 9].map(Color::Ansi),
                age: [2, 6, 3, 9, 1].map(Color::Ansi),
                shades: false,
            },
            "no-color" => Pack {
                name: name.to_owned(),
                dark: true,
                roles: Roles {
                    base: Color::Reset,
                    mantle: Color::Reset,
                    crust: Color::Reset,
                    surface1: Color::Reset,
                    surface2: Color::Reset,
                    overlay1: Color::Reset,
                    subtext: Color::Reset,
                    text: Color::Reset,
                    accent: Color::Reset,
                    title: Color::Reset,
                    link: Color::Reset,
                    red: Color::Reset,
                    green: Color::Reset,
                },
                classes: [Color::Reset; 7],
                cycle: [Color::Reset; 10],
                age: [Color::Reset; 5],
                shades: true,
            },
            _ => return None,
        };
        Some(pack)
    }

    /// The colour of `class`.
    pub fn class(&self, class: Class) -> Color {
        let i = Class::ALL.iter().position(|&c| c == class).unwrap_or(6);
        self.classes[i]
    }

    /// Where a class colour equals a role drawn as a border or fill: each
    /// clash as `role = class`. Empty for every built-in pack; a theme file
    /// with a clash is refused.
    pub fn clashes(&self) -> Vec<String> {
        let drawn = [
            ("link", self.roles.link),
            ("overlay1", self.roles.overlay1),
            ("surface1", self.roles.surface1),
            ("surface2", self.roles.surface2),
            ("accent", self.roles.accent),
        ];
        let mut clashes = Vec::new();
        for (role, color) in drawn {
            for (class, &class_color) in Class::ALL.iter().zip(&self.classes) {
                if color != Color::Reset && color == class_color {
                    clashes.push(format!("{role} = {}", class.as_str()));
                }
            }
        }
        clashes
    }

    /// A pack from a user's theme file (`docs/rules.md`): every colour
    /// `#rrggbb`, thirteen roles, seven classes, ten cycle and five age
    /// colours, no role drawn as a border equal to a class.
    ///
    /// # Errors
    ///
    /// The first thing wrong with the file, as a [`TreemapError`] naming
    /// `name`: its syntax, a missing or unknown role or class, a colour
    /// that is not `#rrggbb`, a list of the wrong length, or a clash.
    pub fn from_toml(text: &str, name: &str) -> Result<Pack, TreemapError> {
        let file: ThemeFile = toml::from_str(text).map_err(|e| TreemapError::ThemeSyntax {
            file: name.to_owned(),
            message: e.message().to_owned(),
        })?;
        let color = |what: &str, hex: &str| {
            Color::hex(hex).ok_or_else(|| TreemapError::ThemeColor {
                file: name.to_owned(),
                key: what.to_owned(),
                value: hex.to_owned(),
            })
        };
        let missing = |table: &'static str, key: &str| TreemapError::ThemeMissing {
            file: name.to_owned(),
            table,
            key: key.to_owned(),
        };
        let role = |key: &str| {
            let hex = file.roles.get(key).ok_or_else(|| missing("roles", key))?;
            color(key, hex)
        };
        if let Some(extra) = file
            .roles
            .keys()
            .find(|k| !ROLE_NAMES.contains(&k.as_str()))
        {
            return Err(TreemapError::ThemeUnknown {
                file: name.to_owned(),
                table: "roles",
                kind: "role",
                key: extra.clone(),
            });
        }
        let roles = Roles {
            base: role("base")?,
            mantle: role("mantle")?,
            crust: role("crust")?,
            surface1: role("surface1")?,
            surface2: role("surface2")?,
            overlay1: role("overlay1")?,
            subtext: role("subtext")?,
            text: role("text")?,
            accent: role("accent")?,
            title: role("title")?,
            link: role("link")?,
            red: role("red")?,
            green: role("green")?,
        };
        let mut classes = [Color::Reset; 7];
        for (slot, class) in classes.iter_mut().zip(Class::ALL) {
            let hex = file
                .classes
                .get(class.as_str())
                .ok_or_else(|| missing("classes", class.as_str()))?;
            *slot = color(class.as_str(), hex)?;
        }
        if let Some(extra) = file
            .classes
            .keys()
            .find(|k| !Class::ALL.iter().any(|c| c.as_str() == k.as_str()))
        {
            return Err(TreemapError::ThemeUnknown {
                file: name.to_owned(),
                table: "classes",
                kind: "class",
                key: extra.clone(),
            });
        }
        let count = |list: &'static str, expected: usize, found: usize| TreemapError::ThemeCount {
            file: name.to_owned(),
            list,
            expected,
            found,
        };
        let cycle: [Color; 10] = colors(&file.cycle, |hex| color("cycle", hex))?
            .try_into()
            .map_err(|v: Vec<Color>| count("cycle", 10, v.len()))?;
        let age: [Color; 5] = colors(&file.age, |hex| color("age", hex))?
            .try_into()
            .map_err(|v: Vec<Color>| count("age", 5, v.len()))?;
        let pack = Pack {
            name: name.to_owned(),
            dark: roles.base.luminance().is_none_or(|l| l < 0.5),
            roles,
            classes,
            cycle,
            age,
            shades: false,
        };
        let conflicts = pack.clashes();
        if !conflicts.is_empty() {
            return Err(TreemapError::ThemeClash {
                file: name.to_owned(),
                clashes: conflicts,
            });
        }
        Ok(pack)
    }
}

/// `hexes` as colours, through `color`.
fn colors(
    hexes: &[String],
    color: impl Fn(&str) -> Result<Color, TreemapError>,
) -> Result<Vec<Color>, TreemapError> {
    hexes.iter().map(|hex| color(hex)).collect()
}

/// The thirteen roles a theme file names.
const ROLE_NAMES: [&str; 13] = [
    "base", "mantle", "crust", "surface1", "surface2", "overlay1", "subtext", "text", "accent",
    "title", "link", "red", "green",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    cycle: Vec<String>,
    age: Vec<String>,
    roles: BTreeMap<String, String>,
    classes: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{NAMES, Pack};
    use crate::TreemapError;
    use crate::cells::Color;

    #[test]
    fn p1_map_02_no_pack_draws_a_border_in_a_class_colour() {
        for name in NAMES {
            let pack = Pack::named(name).unwrap();
            assert_eq!(pack.clashes(), Vec::<String>::new(), "{name}");
            // The class colours are distinct from one another.
            let mut seen = pack.classes.to_vec();
            seen.sort_by_key(|c| format!("{c:?}"));
            seen.dedup();
            assert!(pack.shades || seen.len() == 7, "{name}");
        }
        assert_eq!(Pack::named("gruvbox-dark").unwrap().name, "gruvbox");
        assert!(Pack::named("solarized").is_none());
    }

    #[test]
    fn p1_map_02_the_theme_example_in_docs_rules_md_is_mocha() {
        let doc = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/rules.md"),
        )
        .unwrap();
        let start = doc.find("```toml theme\n").unwrap() + "```toml theme\n".len();
        let end = start + doc[start..].find("```").unwrap();
        let pack = Pack::from_toml(&doc[start..end], "docs").unwrap();
        let mocha = Pack::named("mocha").unwrap();
        assert_eq!(pack.roles, mocha.roles);
        assert_eq!(pack.classes, mocha.classes);
        assert_eq!(pack.cycle, mocha.cycle);
        assert_eq!(pack.age, mocha.age);
        assert!(pack.dark);
        // A clash, a bad colour and a missing role are refused.
        let clash = doc[start..end].replace("link = \"#74c7ec\"", "link = \"#89b4fa\"");
        assert_eq!(
            Pack::from_toml(&clash, "t").unwrap_err(),
            TreemapError::ThemeClash {
                file: "t".to_owned(),
                clashes: vec!["link = code".to_owned()],
            }
        );
        let bad = doc[start..end].replace("\"#1e1e2e\"", "\"blue\"");
        let err = Pack::from_toml(&bad, "t").unwrap_err();
        assert!(matches!(err, TreemapError::ThemeColor { .. }), "{err}");
        assert_eq!(err.to_string(), "t: base = \"blue\" is not #rrggbb");
        let missing = doc[start..end].replace("green = \"#a6e3a1\"\n", "");
        assert_eq!(
            Pack::from_toml(&missing, "t").unwrap_err().to_string(),
            "t: [roles] has no green"
        );
        // A list of the wrong length says how long it should be.
        let short = doc[start..end].replace("\"#f38ba8\"]", "]");
        let err = Pack::from_toml(&short, "t").unwrap_err();
        assert!(
            matches!(
                err,
                TreemapError::ThemeCount {
                    list: "age",
                    expected: 5,
                    found: 4,
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(Color::hex("#a6e3a1"), Some(pack.roles.green));
    }
}
