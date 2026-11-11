//! The command lines that show an entry in the desktop's file manager: the
//! entry selected in the folder that holds it, never opened. A path handed
//! to the system's opener (`open`, `xdg-open`) is opened with its default
//! handler, so an application, a script or an installer would run; these
//! reveal it instead, a folder as much as a file.
//!
//! - macOS: Finder's reveal, `open -R`.
//! - Elsewhere: the freedesktop file manager interface's `ShowItems`
//!   (`org.freedesktop.FileManager1`, which Nautilus, Dolphin, Nemo and
//!   Thunar own), called through `dbus-send`; then, when that cannot
//!   start or the call fails (no session bus, or no file manager owns or can
//!   start the name), `xdg-open` on the folder that holds the entry, which
//!   opens that folder, never the entry.
//!
//! Both are built on every platform, and `super::reveal_commands` picks one
//! with `cfg!`, so the command lines of either are built and tested on the
//! other too.
//!
//! A path that is not absolute gets no command line: the path comes from a
//! scan's root, which a saved scan names for itself and any file may claim,
//! and `open` would read `-a/X.app` as its `-a` option, `xdg-open` a
//! `https://` path as a URL, and a relative one names whatever the working
//! directory holds.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Finder's reveal: `open -R <path>`; none for a path that is not
/// absolute.
pub(super) fn finder(path: &Path) -> Vec<Vec<OsString>> {
    if !path.is_absolute() {
        return Vec::new();
    }
    vec![vec!["open".into(), "-R".into(), path.into()]]
}

/// The freedesktop file manager interface's `ShowItems`, through
/// `dbus-send`, then `xdg-open` on the folder holding `path` (`path` itself
/// when nothing holds it: the root); none for a path that is not absolute.
///
/// `--print-reply` makes `dbus-send` wait for the answer, so a call that
/// fails exits with a failure and the folder is opened instead. The startup
/// id is empty: a terminal program has none to pass on.
pub(super) fn freedesktop(path: &Path) -> Vec<Vec<OsString>> {
    if !path.is_absolute() {
        return Vec::new();
    }
    let folder = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(path);
    vec![
        vec![
            "dbus-send".into(),
            "--session".into(),
            "--print-reply".into(),
            "--type=method_call".into(),
            "--dest=org.freedesktop.FileManager1".into(),
            "/org/freedesktop/FileManager1".into(),
            "org.freedesktop.FileManager1.ShowItems".into(),
            format!("array:string:{}", file_uri(path)).into(),
            "string:".into(),
        ],
        vec!["xdg-open".into(), folder.into()],
    ]
}

/// `path` as a `file://` URI, every byte but `/` and RFC 3986's unreserved
/// characters percent-encoded: a name need not be UTF-8, which a D-Bus
/// string must be, and a comma would split `dbus-send`'s array in two.
fn file_uri(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    //! P4-TUI-05: `o` reveals the selection and never opens it, on both
    //! platforms' command lines, each built here whatever the platform.

    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::{file_uri, finder, freedesktop};

    /// The command lines as strings, for comparison.
    fn lines(commands: &[Vec<OsString>]) -> Vec<Vec<String>> {
        commands
            .iter()
            .map(|line| {
                line.iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }

    /// The `dbus-send` call that shows the item `uri`.
    fn show_items(uri: &str) -> Vec<String> {
        [
            "dbus-send",
            "--session",
            "--print-reply",
            "--type=method_call",
            "--dest=org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{uri}"),
            "string:",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn p4_tui_05_macos_reveals_the_selection_with_open_r() {
        for path in [
            "/Users/alex/Downloads/Installer.pkg",
            "/Users/alex/Downloads/Tool.app",
            "/Users/alex/Downloads/run.command",
        ] {
            assert_eq!(
                lines(&finder(Path::new(path))),
                [["open", "-R", path].map(String::from).to_vec()]
            );
        }
    }

    #[test]
    fn p4_tui_05_linux_shows_the_item_then_falls_back_to_its_folder() {
        // A file: selected in its folder, else the folder opened.
        assert_eq!(
            lines(&freedesktop(Path::new("/home/alex/Downloads/setup.sh"))),
            [
                show_items("file:///home/alex/Downloads/setup.sh"),
                ["xdg-open", "/home/alex/Downloads"]
                    .map(String::from)
                    .to_vec(),
            ]
        );
        // A folder is revealed the same way, in the folder holding it.
        assert_eq!(
            lines(&freedesktop(Path::new("/home/alex/Downloads"))),
            [
                show_items("file:///home/alex/Downloads"),
                ["xdg-open", "/home/alex"].map(String::from).to_vec(),
            ]
        );
        // The root has no folder above it: the root itself.
        assert_eq!(
            lines(&freedesktop(Path::new("/"))),
            [
                show_items("file:///"),
                ["xdg-open", "/"].map(String::from).to_vec(),
            ]
        );
    }

    #[test]
    fn p4_tui_05_no_command_line_hands_the_entry_to_an_opener() {
        let entry = Path::new("/home/alex/Downloads/Tool.AppImage");
        for commands in [finder(entry), freedesktop(entry)] {
            for line in &commands {
                let (program, args) = line.split_first().unwrap();
                let opens_entry = args.iter().any(|arg| arg == entry.as_os_str());
                let reveals = args.first().is_some_and(|arg| arg == "-R");
                assert!(!opens_entry || (program == "open" && reveals), "{line:?}");
            }
        }
    }

    #[test]
    fn p4_tui_05_the_file_uri_keeps_any_name_in_one_array_element() {
        // Spaces, a comma (dbus-send's separator), `%`, `#`, `?` and bytes
        // that are not UTF-8 are encoded; `/` and the unreserved stay.
        let name = OsStr::from_bytes(b"/tmp/a b,c%d#e?f\xff\xc3\xa9~g-h_i.j");
        assert_eq!(
            file_uri(Path::new(name)),
            "file:///tmp/a%20b%2Cc%25d%23e%3Ff%FF%C3%A9~g-h_i.j"
        );
        let shown = &freedesktop(Path::new(name))[0][7];
        assert!(shown.to_str().is_some_and(|arg| !arg[13..].contains(',')));
    }

    #[test]
    fn p4_tui_05_a_path_that_is_not_absolute_gets_no_command_line() {
        // An option to `open` (`-a` names the application to open with,
        // `-R` would reveal its argument), a URL to `xdg-open`, or a name
        // the working directory resolves: none reaches a command line.
        for path in ["-a/X.app", "-R", "https://host/x", "Downloads/x", "x", ""] {
            let path = Path::new(path);
            assert_eq!(finder(path), Vec::<Vec<OsString>>::new(), "{path:?}");
            assert_eq!(freedesktop(path), Vec::<Vec<OsString>>::new(), "{path:?}");
            assert!(crate::reveal_commands(path).is_empty(), "{path:?}");
        }
    }

    #[test]
    fn p4_tui_05_this_platform_reveals_with_its_own_command_lines() {
        let path = Path::new("/srv/data/report.pdf");
        let expected = if cfg!(target_os = "macos") {
            finder(path)
        } else {
            freedesktop(path)
        };
        assert_eq!(crate::reveal_commands(path), expected);
    }
}
