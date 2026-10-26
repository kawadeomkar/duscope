//! P0-DOC-01: `docs/README.md`, the documentation index, links every document
//! in `docs/`. The docs job's link check proves the links resolve; only this
//! test sees one missing.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

#[test]
fn p0_doc_01_docs_index_links_every_document() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs");
    let index = fs::read_to_string(docs.join("README.md")).unwrap();
    let mut unlinked: Vec<String> = fs::read_dir(&docs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                && name != "README.md"
        })
        .filter(|name| !index.contains(&format!("]({name})")))
        .collect();
    unlinked.sort();
    assert!(
        unlinked.is_empty(),
        "docs/README.md does not link {unlinked:?}"
    );
}
