use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use super::{Config, ConfigSource, Setting};
use crate::CoreError;

fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| OsString::from(v))
    }
}

#[test]
fn p2_core_02_the_file_is_the_flag_then_duscope_config_then_xdg() {
    let all = [
        ("DUSCOPE_CONFIG", "/env/config.toml"),
        ("XDG_CONFIG_HOME", "/xdg"),
        ("HOME", "/home/me"),
    ];
    let flag = Some(OsStr::new("/flag/config.toml"));
    assert_eq!(
        ConfigSource::locate(flag, vars(&all)),
        ConfigSource::Named(PathBuf::from("/flag/config.toml"))
    );
    assert_eq!(
        ConfigSource::locate(None, vars(&all)),
        ConfigSource::Named(PathBuf::from("/env/config.toml"))
    );
    assert_eq!(
        ConfigSource::locate(None, vars(&all[1..])),
        ConfigSource::Default(PathBuf::from("/xdg/duscope/config.toml"))
    );
    // HOME's .config when XDG_CONFIG_HOME is unset, empty or relative.
    let home = PathBuf::from("/home/me/.config/duscope/config.toml");
    for xdg in [None, Some(""), Some("relative/dir")] {
        let mut set = vec![("HOME", "/home/me")];
        set.extend(xdg.map(|v| ("XDG_CONFIG_HOME", v)));
        assert_eq!(
            ConfigSource::locate(None, vars(&set)),
            ConfigSource::Default(home.clone()),
            "XDG_CONFIG_HOME={xdg:?}"
        );
    }
    // `none` reads no file, from the flag or the variable.
    assert_eq!(
        ConfigSource::locate(Some(OsStr::new("none")), vars(&all)),
        ConfigSource::None
    );
    assert_eq!(
        ConfigSource::locate(None, vars(&[("DUSCOPE_CONFIG", "none"), ("HOME", "/h")])),
        ConfigSource::None
    );
    assert_eq!(ConfigSource::locate(None, vars(&[])), ConfigSource::None);
}

#[test]
fn p2_core_02_a_missing_default_file_is_no_configuration() {
    let env = duscope_testkit::TestEnv::new();
    let missing = env.xdg_config_home().join("duscope/config.toml");
    assert_eq!(
        Config::load(&ConfigSource::Default(missing.clone())).unwrap(),
        Config::default()
    );
    assert_eq!(
        Config::load(&ConfigSource::None).unwrap(),
        Config::default()
    );
    // A file the user named must be there.
    let error = Config::load(&ConfigSource::Named(missing.clone())).unwrap_err();
    assert!(matches!(error, CoreError::ConfigRead { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with(&missing.display().to_string())
    );
}

#[test]
fn p2_core_02_every_key_is_read_with_its_line() {
    let text = r#"
[defaults]
threads = 6
theme = "dracula"
color_by = "age"
format = "json"
limit = 40
size = "apparent"
attrs = "full"
clones = false
max_age = "3d"
index_dir = "indexes"

[excludes]
patterns = ["*.tmp", "build/"]

[classes]
extra = "classes.toml"

[caches]
extra = "/etc/duscope/caches.toml"
"#;
    let config = Config::parse(text, Path::new("/conf/duscope/config.toml")).unwrap();
    let d = &config.defaults;
    assert_eq!(d.threads, Some(Setting { value: 6, line: 3 }));
    assert_eq!(d.theme.as_ref().unwrap().value, "dracula");
    assert_eq!(d.color_by.as_ref().unwrap().value, "age");
    assert_eq!(d.format.as_ref().unwrap().value, "json");
    assert_eq!(d.limit.as_ref().unwrap().value, 40);
    assert_eq!(d.size.as_ref().unwrap().line, 8);
    assert_eq!(d.attrs.as_ref().unwrap().value, "full");
    assert_eq!(
        d.clones,
        Some(Setting {
            value: false,
            line: 10
        })
    );
    assert_eq!(d.max_age.as_ref().unwrap().value, "3d");
    // Relative paths are the file's directory's.
    assert_eq!(
        d.index_dir.as_ref().unwrap().value,
        Path::new("/conf/duscope/indexes")
    );
    assert_eq!(config.excludes, ["*.tmp", "build/"]);
    assert_eq!(
        config.classes_extra.as_deref(),
        Some(Path::new("/conf/duscope/classes.toml"))
    );
    assert_eq!(
        config.caches_extra.as_deref(),
        Some(Path::new("/etc/duscope/caches.toml"))
    );
    assert_eq!(config.at(3), "/conf/duscope/config.toml:3");
}

#[test]
fn p2_core_02_a_theme_path_is_the_files_directorys() {
    let at = Path::new("/conf/duscope/config.toml");
    let path = Config::parse("[defaults]\ntheme = \"themes/mine.toml\"\n", at).unwrap();
    assert_eq!(
        path.defaults.theme.unwrap().value,
        "/conf/duscope/themes/mine.toml"
    );
    let name = Config::parse("[defaults]\ntheme = \"mocha\"\n", at).unwrap();
    assert_eq!(name.defaults.theme.unwrap().value, "mocha");
}

#[test]
fn p2_core_02_an_unknown_key_or_a_wrong_type_names_its_line() {
    let at = Path::new("/c/config.toml");
    let unknown = Config::parse("[defaults]\nthreads = 2\ncolour = \"age\"\n", at).unwrap_err();
    assert!(
        matches!(unknown, CoreError::Config { line: 3, .. }),
        "{unknown:?}"
    );
    assert!(
        unknown.to_string().starts_with("/c/config.toml:3: "),
        "{unknown}"
    );
    let table = Config::parse("[colors]\nby = \"age\"\n", at).unwrap_err();
    assert!(
        matches!(table, CoreError::Config { line: 1, .. }),
        "{table:?}"
    );
    let wrong = Config::parse("[defaults]\n\nlimit = \"many\"\n", at).unwrap_err();
    assert!(
        matches!(wrong, CoreError::Config { line: 3, .. }),
        "{wrong:?}"
    );
}
