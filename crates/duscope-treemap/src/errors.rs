//! The crate's error enum.

/// Why a treemap input was refused: so far, a user's theme file
/// ([`crate::packs::Pack::from_toml`]). Each variant names the file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreemapError {
    /// The file is not TOML, or not in the theme file's shape.
    #[error("{file}: {message}")]
    ThemeSyntax {
        /// The theme file.
        file: String,
        /// What the parser found.
        message: String,
    },
    /// A role or class the file does not name.
    #[error("{file}: [{table}] has no {key}")]
    ThemeMissing {
        /// The theme file.
        file: String,
        /// `roles` or `classes`.
        table: &'static str,
        /// The role or class.
        key: String,
    },
    /// A key that is not one of the thirteen roles or seven classes.
    #[error("{file}: [{table}] has no {kind} {key:?}")]
    ThemeUnknown {
        /// The theme file.
        file: String,
        /// `roles` or `classes`.
        table: &'static str,
        /// `role` or `class`.
        kind: &'static str,
        /// The key as written.
        key: String,
    },
    /// A colour that is not `#rrggbb`.
    #[error("{file}: {key} = {value:?} is not #rrggbb")]
    ThemeColor {
        /// The theme file.
        file: String,
        /// The role, class or list it is given for.
        key: String,
        /// The value as written.
        value: String,
    },
    /// A `cycle` or `age` list of the wrong length.
    #[error("{file}: {list} needs {expected} colours, not {found}")]
    ThemeCount {
        /// The theme file.
        file: String,
        /// `cycle` or `age`.
        list: &'static str,
        /// The colours it needs.
        expected: usize,
        /// The colours it has.
        found: usize,
    },
    /// A role drawn as a border or fill has a class's colour.
    #[error(
        "{file}: a role drawn as a border or fill equals a class colour: {}",
        clashes.join(", ")
    )]
    ThemeClash {
        /// The theme file.
        file: String,
        /// Each clash as `role = class`.
        clashes: Vec<String>,
    },
}
