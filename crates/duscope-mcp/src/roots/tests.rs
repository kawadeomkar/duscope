//! P3-MCP-03: the roots helper's refusals and what it lets through.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;

use super::*;

/// A temporary tree: `root/{inside/file, outside_link -> ../elsewhere}` and
/// `elsewhere/secret`, all canonical.
struct Tree {
    _env: TestEnv,
    base: PathBuf,
}

impl Tree {
    fn new() -> Tree {
        let env = TestEnv::new();
        let base = env.work_dir().canonicalize().unwrap();
        fs::create_dir_all(base.join("root/inside")).unwrap();
        fs::write(base.join("root/inside/file"), b"x").unwrap();
        fs::create_dir_all(base.join("elsewhere")).unwrap();
        fs::write(base.join("elsewhere/secret"), b"x").unwrap();
        fs::create_dir_all(base.join("rootish")).unwrap();
        symlink(base.join("elsewhere"), base.join("root/outside_link")).unwrap();
        symlink(base.join("root/inside"), base.join("root/inside_link")).unwrap();
        Tree { _env: env, base }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }

    fn roots(&self) -> Roots {
        Roots::new([self.path("root")]).unwrap()
    }
}

fn refused(result: Result<PathBuf, Refusal>) -> Refusal {
    let refusal = result.expect_err("the path should be refused");
    assert_eq!(refusal.code, "refused_path", "{refusal}");
    refusal
}

#[test]
fn p3_mcp_03_roots_refuse_an_escape_by_dot_dot() {
    let tree = Tree::new();
    let roots = tree.roots();
    let escape = tree.path("root/inside/../../elsewhere/secret");
    let refusal = refused(roots.bound_path(&escape, None));
    assert!(refusal.message.contains("`..`"), "{refusal}");
    // Even one that would stay inside: the rule is the component, not
    // where it leads.
    refused(roots.bound_path(&tree.path("root/inside/../inside/file"), None));
    // Relative to a scan root too.
    refused(roots.bound_path(Path::new("../elsewhere"), Some(&tree.path("root"))));
}

#[test]
fn p3_mcp_03_roots_refuse_a_link_that_leads_out() {
    let tree = Tree::new();
    let roots = tree.roots();
    let refusal = refused(roots.bound_path(&tree.path("root/outside_link/secret"), None));
    assert!(
        refusal
            .message
            .contains(&tree.path("elsewhere/secret").display().to_string()),
        "the message names where the link leads: {refusal}"
    );
    refused(roots.bound_path(&tree.path("root/outside_link"), None));
    // A link that stays inside resolves to its target.
    assert_eq!(
        roots
            .bound_path(&tree.path("root/inside_link/file"), None)
            .unwrap(),
        tree.path("root/inside/file")
    );
}

#[test]
fn p3_mcp_03_roots_refuse_an_index_file_for_a_scan() {
    let tree = Tree::new();
    let roots = tree.roots();
    for selector in [
        "/elsewhere/scans/0123456789ab.db",
        "./0123456789ab.db",
        "../0123456789ab",
        "0123456789ab.db",
        "abc",
        "LATEST",
        "0123456789AB",
        "",
    ] {
        let refusal = roots.scan_selector(selector).expect_err(selector);
        assert_eq!(refusal.code, "refused_path", "{selector}");
    }
    for selector in ["latest", "0123", "0123456789ab"] {
        assert_eq!(roots.scan_selector(selector), Ok(selector));
    }
}

#[test]
fn p3_mcp_03_roots_take_paths_under_them() {
    let tree = Tree::new();
    let roots = tree.roots();
    let root = tree.path("root");
    assert_eq!(roots.bound_path(&root, None).unwrap(), root);
    assert_eq!(
        roots
            .bound_path(&tree.path("root/inside/file"), None)
            .unwrap(),
        tree.path("root/inside/file")
    );
    // Relative to the scan root, and `.` changes nothing.
    assert_eq!(
        roots
            .bound_path(Path::new("./inside/file"), Some(&root))
            .unwrap(),
        tree.path("root/inside/file")
    );
    // A file deleted since the scan is still a path under the root.
    assert_eq!(
        roots
            .bound_path(&tree.path("root/inside/gone/file"), None)
            .unwrap(),
        tree.path("root/inside/gone/file")
    );
}

#[test]
fn p3_mcp_03_roots_refuse_paths_outside_them() {
    let tree = Tree::new();
    let roots = tree.roots();
    refused(roots.bound_path(&tree.path("elsewhere/secret"), None));
    refused(roots.bound_path(&tree.path("elsewhere/gone"), None));
    // A sibling whose name starts with the root's is not under it.
    refused(roots.bound_path(&tree.path("rootish"), None));
    refused(roots.bound_path(&tree.base, None));
    refused(roots.bound_path(Path::new("/"), None));
    // A relative path needs a scan root to be read from.
    let refusal = refused(roots.bound_path(Path::new("inside/file"), None));
    assert!(refusal.message.contains("relative"), "{refusal}");
}

#[test]
fn p3_mcp_03_roots_are_canonical_and_kept_once() {
    let tree = Tree::new();
    let roots = Roots::new([
        tree.path("root/inside_link"),
        tree.path("root/inside"),
        tree.path("elsewhere/../root/inside/."),
        tree.path("elsewhere"),
    ])
    .unwrap();
    assert_eq!(
        roots.paths(),
        [tree.path("elsewhere"), tree.path("root/inside")]
    );
    assert_eq!(roots.default_path(), None);
    assert_eq!(
        tree.roots().default_path(),
        Some(tree.path("root").as_path())
    );
    // Several roots: a path under either passes.
    assert!(
        roots
            .bound_path(&tree.path("elsewhere/secret"), None)
            .is_ok()
    );
    assert!(
        roots
            .bound_path(&tree.path("root/inside/file"), None)
            .is_ok()
    );
    refused(roots.bound_path(&tree.path("root"), None));
}

#[test]
fn p3_mcp_03_a_root_must_be_a_directory_there_is() {
    let tree = Tree::new();
    let missing = Roots::new([tree.path("nowhere")]).unwrap_err();
    assert_eq!(missing.path, tree.path("nowhere"));
    let file = Roots::new([tree.path("root/inside/file")]).unwrap_err();
    assert!(file.to_string().contains("not a directory"), "{file}");
}

#[test]
fn p3_mcp_03_a_saved_scan_is_seen_through_the_roots() {
    let tree = Tree::new();
    let roots = Roots::new([tree.path("root/inside"), tree.path("elsewhere")]).unwrap();
    assert_eq!(
        roots.visibility(&tree.path("root/inside")),
        Visibility::Inside
    );
    assert_eq!(
        roots.visibility(&tree.path("root/inside/deeper")),
        Visibility::Inside
    );
    assert_eq!(
        roots.visibility(&tree.path("root")),
        Visibility::Above(vec![tree.path("root/inside")])
    );
    assert_eq!(
        roots.visibility(&tree.base),
        Visibility::Above(vec![tree.path("elsewhere"), tree.path("root/inside")])
    );
    assert_eq!(roots.visibility(&tree.path("rootish")), Visibility::Hidden);
    assert_eq!(roots.visibility(Path::new("/nowhere")), Visibility::Hidden);
}

#[test]
fn p3_mcp_03_a_scan_id_no_saved_scan_has_is_not_found() {
    let tree = Tree::new();
    let refusal = tree
        .roots()
        .saved_scan(&tree.path("no-index-dir"), "0123")
        .unwrap_err();
    assert_eq!(refusal.code, "scan_not_found", "{refusal}");
}

#[test]
fn p5_mcp_01_an_item_whose_folder_became_a_link_out_of_the_roots_is_refused() {
    let tree = Tree::new();
    let roots = tree.roots();
    let item = tree.path("root/inside/file");
    assert_eq!(roots.bound_item(&item), Ok(()));
    // After a handle named it, its folder moved out of the roots and a
    // link to it took its place: the same path, and the same entry, now
    // outside them.
    fs::rename(tree.path("root/inside"), tree.path("elsewhere/inside")).unwrap();
    symlink(tree.path("elsewhere/inside"), tree.path("root/inside")).unwrap();
    let refusal = roots.bound_item(&item).expect_err("the item lies outside");
    assert_eq!(refusal.code, "refused_path", "{refusal}");
    assert!(
        refusal.message.contains(&format!(
            "resolves to {}",
            tree.path("elsewhere/inside/file").display()
        )),
        "{refusal}"
    );
    // The link itself lies inside, and is judged there.
    assert_eq!(roots.bound_item(&tree.path("root/inside")), Ok(()));
}

// P5-SEC-05: a path's length, and what resolving one costs.

/// A path of exactly `bytes` bytes under `base`, of one-letter parts that do
/// not exist.
fn long_path(base: &Path, bytes: usize) -> PathBuf {
    let base = base.as_os_str().to_str().unwrap();
    let mut path = format!("{base}/");
    while path.len() + 2 <= bytes {
        path.push_str("a/");
    }
    if path.len() < bytes {
        path.push('b');
    } else {
        path.pop();
        path.push('b');
    }
    assert_eq!(path.len(), bytes);
    PathBuf::from(path)
}

#[test]
fn p5_sec_05_a_path_past_the_limit_is_refused_by_its_length() {
    let tree = Tree::new();
    let roots = tree.roots();
    // As long as a message allows: bisection or not, it is refused before
    // anything looks at it.
    for bytes in [MAX_PATH_BYTES + 1, crate::transport::MAX_MESSAGE_BYTES] {
        let path = long_path(&tree.path("root"), bytes);
        let refused = roots.bound_path(&path, None).unwrap_err();
        assert_eq!(refused.code, "refused_path");
        assert!(
            refused.message.contains(&format!("{bytes} bytes")),
            "{refused}"
        );
        assert!(
            refused.message.len() < 200,
            "the path is not repeated: {refused}"
        );
        let refused = roots.bound_item(&path).unwrap_err();
        assert_eq!(refused.code, "refused_path");
        assert!(
            refused.message.contains(&format!("{bytes} bytes")),
            "{refused}"
        );
    }
}

#[test]
fn p5_sec_05_a_path_at_the_limit_is_read() {
    let tree = Tree::new();
    let roots = tree.roots();
    let path = long_path(&tree.path("root"), MAX_PATH_BYTES);
    let bound = roots.bound_path(&path, None).unwrap();
    assert!(bound.starts_with(tree.path("root")), "{}", bound.display());
    assert_eq!(bound.components().count(), path.components().count());
}

#[test]
fn p5_sec_05_a_relative_path_is_measured_once_joined_to_its_scan_root() {
    let tree = Tree::new();
    let roots = tree.roots();
    let root = tree.path("root");
    let relative = "a/".repeat(MAX_PATH_BYTES / 2 - 1);
    assert!(relative.len() < MAX_PATH_BYTES);
    assert!(root.join(&relative).as_os_str().len() > MAX_PATH_BYTES);
    let refused = roots
        .bound_path(Path::new(&relative), Some(&root))
        .unwrap_err();
    assert_eq!(refused.code, "refused_path");
    assert!(refused.message.contains("bytes long"), "{refused}");
}

#[test]
fn p5_sec_05_resolving_a_path_takes_a_logarithmic_number_of_looks() {
    // A path of 2,000 parts, of which the first five exist.
    let path = PathBuf::from(format!("/{}", vec!["d"; 2000].join("/")));
    let exists = PathBuf::from("/d/d/d/d/d");
    let mut looks = 0;
    let resolved = resolve_with(&path, |part| {
        looks += 1;
        if exists.starts_with(part) {
            Ok(Path::new("/real").join(part.strip_prefix("/").unwrap()))
        } else {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    });
    assert!(looks <= 13, "{looks} looks for 2,000 parts");
    let tail = vec!["d"; 1995].join("/");
    assert_eq!(resolved, Path::new("/real/d/d/d/d/d").join(tail));
}

#[test]
fn p5_sec_05_resolving_keeps_what_the_tree_says() {
    let tree = Tree::new();
    // Every part exists: no remainder, and nothing is added to it.
    let whole = resolve(&tree.path("root/inside/file"));
    assert_eq!(whole, tree.path("root/inside/file"));
    assert_eq!(whole.as_os_str(), tree.path("root/inside/file").as_os_str());
    // Below a link, as the file system resolves it.
    let below = resolve(&tree.path("root/outside_link/gone/deeper"));
    assert_eq!(below, tree.path("elsewhere/gone/deeper"));
    // Doubled separators and `.` in the remainder are not kept.
    let tidy = resolve(Path::new(&format!(
        "{}//gone/./deeper",
        tree.path("root/inside").display()
    )));
    assert_eq!(tidy, tree.path("root/inside/gone/deeper"));
    assert_eq!(
        tidy.as_os_str(),
        tree.path("root/inside/gone/deeper").as_os_str()
    );
}

#[test]
fn p5_sec_05_a_search_the_tree_changed_under_stops_at_a_part_that_resolves() {
    // The bisection saw the deeper part resolve, and it is gone by the time
    // it is read: the part above it answers, not the path as written.
    let path = Path::new("/a/b/c");
    let mut first = true;
    let resolved = resolve_with(path, |part| {
        if part == Path::new("/a/b") {
            let then = first;
            first = false;
            if then {
                return Ok(PathBuf::from("/real/a/b"));
            }
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        if part == path {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        Ok(Path::new("/real").join(part.strip_prefix("/").unwrap()))
    });
    assert_eq!(resolved, Path::new("/real/a/b/c"));
}
