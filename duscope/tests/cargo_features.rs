//! P0-REPO-06: `pixels` is the only Cargo feature, as `CLAUDE.md`'s invariant says
//! (DEV-174). The binary's `pixels` turns on `duscope-treemap`'s, its half in the
//! component; any other feature in a workspace manifest is a component flag.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;

use cargo_metadata::MetadataCommand;

#[test]
fn p0_repo_06_pixels_is_the_only_cargo_feature() {
    let metadata = MetadataCommand::new()
        .manifest_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml"))
        .no_deps()
        .exec()
        .unwrap();
    let mut others = Vec::new();
    for package in metadata.workspace_packages() {
        for feature in package.features.keys() {
            if feature != "pixels" {
                others.push(format!("{} defines `{feature}`", package.name));
            }
        }
    }
    assert!(
        others.is_empty(),
        "a feature other than `pixels` (an optional dependency named without `dep:` is one too):\n{}",
        others.join("\n")
    );
    let binary = metadata
        .workspace_packages()
        .into_iter()
        .find(|package| package.name.as_str() == "duscope")
        .unwrap();
    assert_eq!(
        binary.features.get("pixels").map(Vec::as_slice),
        Some(["duscope-treemap/pixels".to_owned()].as_slice()),
        "the binary's `pixels` turns on duscope-treemap's"
    );
}
