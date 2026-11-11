//! P4-TUI-06: `walk_differs_below`, where the Data volume's rule has
//! nothing to decide: the platform-neutral answer. Its macOS decisions are
//! tested by the firmlink table in `src/platform/macos/tests.rs`; a bind
//! cycle, which also rests on where a walk starts, is the saved scan's to
//! tell (`duscope/src/verbs/rescan.rs`). No path here is walked.

use std::path::Path;

use duscope_walk::{WalkOptions, walk_differs_below};

#[test]
fn p4_tui_06_a_folder_with_no_way_in_below_it_walks_as_its_root_does() {
    let opts = WalkOptions::default();
    for (root, folder) in [("/tree", "/tree/proj"), ("/", "/nowhere/proj")] {
        assert!(
            !walk_differs_below(Path::new(root), Path::new(folder), &opts),
            "{root} {folder}"
        );
    }
}

#[test]
fn p4_tui_06_off_macos_no_firmlink_rule_sets_a_folder_apart() {
    if cfg!(not(target_os = "macos")) {
        let opts = WalkOptions::default();
        for folder in ["/System", "/System/Volumes", "/System/Library"] {
            assert!(!walk_differs_below(
                Path::new("/"),
                Path::new(folder),
                &opts
            ));
        }
    }
}
