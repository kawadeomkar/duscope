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
