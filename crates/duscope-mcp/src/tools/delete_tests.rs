//! P5-MCP-01: `duscope_delete` as the registry sees it: the arguments its
//! schema takes, the command line they make, and what it refuses before its
//! verb runs. That the verb acts as asked, and what a client receives, are
//! the conformance suite's (`tests/conformance.rs`).

use std::path::Path;

use duscope_core::handle::{Handle, HandlePayload, Key};
use duscope_testkit::TestEnv;
use serde_json::json;

use super::scan_group_tests::command_line;
use super::*;

/// A handle, signed with the key in `index_dir`, of `path`, good for a
/// quarter of an hour.
fn handle_of(index_dir: &Path, path: &Path) -> String {
    let key = Key::open(index_dir).unwrap();
    let meta = std::fs::symlink_metadata(path).ok();
    let payload = HandlePayload {
        path: path.as_os_str().as_encoded_bytes().to_vec(),
        dev: meta.as_ref().map_or(0, std::os::unix::fs::MetadataExt::dev),
        ino: meta.as_ref().map_or(0, std::os::unix::fs::MetadataExt::ino),
        allocated: 4096,
        scan_id: "3f9a0c1d2e4b".to_owned(),
        expires_at: duscope_core::query::clock::now() + duscope_core::handle::LIFETIME_NS,
    };
    Handle::issue(&key, &payload)
}

/// The tool's schema, read as the registry reads it.
fn delete_schema() -> Schema {
    let tool = shipped()
        .into_iter()
        .find(|t| t.name() == "duscope_delete")
        .unwrap();
    Schema::read(&serde_json::from_str(tool.input_schema()).unwrap()).unwrap()
}

/// The first argument refused, or `None`.
fn checked(arguments: &Value) -> Option<String> {
    delete_schema()
        .check("duscope_delete", arguments.as_object().unwrap())
        .err()
        .map(|refusal| {
            assert_eq!(refusal.code, "usage", "{refusal}");
            refusal.message
        })
}

#[test]
fn p5_mcp_01_handle_and_expect_bytes_are_mandatory_on_every_item() {
    let good = json!({"handle": "aA0-_", "expect_bytes": 10});
    assert_eq!(checked(&json!({"items": [good]})), None);
    assert_eq!(
        checked(&json!({"items": [good, good], "dry_run": false})),
        None
    );
    // No items, no handle, or no expectation: refused before anything runs.
    assert!(checked(&json!({})).unwrap().contains("items is required"));
    for items in [
        json!([]),
        json!([{"expect_bytes": 1}]),
        json!([{"handle": "x"}]),
        json!([{"handle": "x", "expect_bytes": -1}]),
        json!([{"handle": "x", "expect_bytes": 1.5}]),
        json!([{"handle": "not base64!", "expect_bytes": 1}]),
        json!([{"handle": "", "expect_bytes": 1}]),
        // Neither a path nor a way to say anything but the Trash.
        json!([{"handle": "x", "expect_bytes": 1, "permanent": true}]),
        json!([{"path": "/x", "expect_bytes": 1}]),
        json!(["x"]),
    ] {
        assert!(
            checked(&json!({ "items": items }))
                .unwrap_or_else(|| panic!("{items}"))
                .contains("the argument items must be"),
            "{items}"
        );
    }
    let many = vec![good.clone(); 101];
    assert!(
        checked(&json!({ "items": many }))
            .unwrap()
            .contains("items")
    );
    for bad in ["yes", "1"] {
        assert!(
            checked(&json!({"items": [good], "dry_run": bad})).is_some(),
            "{bad}"
        );
    }
    assert!(
        checked(&json!({"items": [good], "permanent": true}))
            .unwrap()
            .contains("unknown argument permanent")
    );
}

#[test]
fn p5_mcp_01_each_item_becomes_a_handle_and_its_expected_bytes() {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap();
    std::fs::write(root.join("one.bin"), b"1").unwrap();
    std::fs::write(root.join("two.bin"), b"2").unwrap();
    let index_dir = env.index_dir();
    let one = handle_of(&index_dir, &root.join("one.bin"));
    let two = handle_of(&index_dir, &root.join("two.bin"));
    let line = |arguments: &Value| -> Vec<String> {
        command_line("duscope_delete", arguments, &root, &index_dir)
    };
    let items = json!([
        {"handle": one, "expect_bytes": 4096},
        {"handle": two, "expect_bytes": 8192},
    ]);
    let from = |h: &str| format!("--from={h}");
    let expect = |n: u64| format!("--expect-item-bytes={n}");
    // A dry run by default, which only --yes ends; never --permanent.
    for arguments in [
        json!({ "items": items }),
        json!({"items": items, "dry_run": true}),
    ] {
        assert_eq!(
            line(&arguments),
            [
                "clean".to_owned(),
                from(&one),
                expect(4096),
                from(&two),
                expect(8192),
                "--dry-run".to_owned(),
            ]
        );
    }
    let moved = line(&json!({"items": items, "dry_run": false}));
    assert_eq!(moved.last().unwrap(), "--yes");
    assert!(
        !moved.iter().any(|arg| arg.contains("permanent")),
        "{moved:?}"
    );
    assert!(!moved.iter().any(|arg| arg == "--dry-run"), "{moved:?}");
    // A handle is verified before the command line is made, so one that
    // reads like a flag is refused as the handle it is not.
    let refusal = invocation_refusal(
        &json!({"items": [{"handle": "--permanent", "expect_bytes": 1}]}),
        &root,
        &index_dir,
    );
    assert_eq!(refusal.code, "handle_invalid", "{refusal}");
}

/// The refusal `duscope_delete` answers `arguments` with.
fn invocation_refusal(arguments: &Value, root: &Path, index_dir: &Path) -> Refusal {
    let roots = Roots::new([root]).unwrap();
    let context = Context {
        roots: &roots,
        index_dir,
    };
    shipped()
        .into_iter()
        .find(|t| t.name() == "duscope_delete")
        .unwrap()
        .invocation(arguments.as_object().unwrap(), &context)
        .unwrap_err()
}

#[test]
fn p5_mcp_01_a_handle_must_verify_and_its_path_lie_under_the_roots() {
    let env = TestEnv::new();
    let base = env.tmp_dir().canonicalize().unwrap();
    let root = base.join("root");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/in.bin"), b"x").unwrap();
    std::fs::write(base.join("out.bin"), b"x").unwrap();
    let index_dir = env.index_dir();
    let call = |handle: &str| json!({"items": [{"handle": handle, "expect_bytes": 1}]});
    let refused = |handle: &str| invocation_refusal(&call(handle), &root, &index_dir);

    // A path outside the roots, from a handle this server's key signed.
    let out = handle_of(&index_dir, &base.join("out.bin"));
    assert_eq!(refused(&out).code, "refused_path");
    // A root itself, though it lies under the scan's root.
    let at_root = handle_of(&index_dir, &root);
    let refusal = refused(&at_root);
    assert_eq!(refusal.code, "refused_path", "{refusal}");
    assert!(refusal.message.contains("one of the roots"), "{refusal}");

    // A link inside a root that leads out is judged where it lies: inside
    // the roots, so the handle passes; `clean`'s plan then refuses the link.
    std::os::unix::fs::symlink(base.join("out.bin"), root.join("out-link")).unwrap();
    let link = handle_of(&index_dir, &root.join("out-link"));
    assert!(
        command_line("duscope_delete", &call(&link), &root, &index_dir)
            .iter()
            .any(|arg| arg == "--dry-run")
    );
    // A link outside that leads in is outside: the handle's own path counts.
    std::os::unix::fs::symlink(root.join("sub/in.bin"), base.join("in-link")).unwrap();
    let refusal = refused(&handle_of(&index_dir, &base.join("in-link")));
    assert_eq!(refusal.code, "refused_path", "{refusal}");

    // Inside the roots, a handle another key signed: not one of this
    // server's handles, whatever it names.
    let other_key = Key::from_bytes([9; 32]);
    let forged = Handle::issue(
        &other_key,
        &HandlePayload {
            path: root
                .join("sub/in.bin")
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
            dev: 1,
            ino: 1,
            allocated: 1,
            scan_id: String::new(),
            expires_at: duscope_core::query::clock::now() + duscope_core::handle::LIFETIME_NS,
        },
    );
    assert_eq!(refused(&forged).code, "handle_invalid");
    assert!(
        refused(&forged).hint.contains("duscope_explain"),
        "{}",
        refused(&forged)
    );

    // One past its expiry.
    let key = Key::open(&index_dir).unwrap();
    let expired = Handle::issue(
        &key,
        &HandlePayload {
            path: root
                .join("sub/in.bin")
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
            dev: 1,
            ino: 1,
            allocated: 1,
            scan_id: String::new(),
            expires_at: 1,
        },
    );
    let refusal = refused(&expired);
    assert_eq!(refusal.code, "handle_expired", "{refusal}");
    assert!(refusal.hint.contains("duscope_explain"), "{refusal}");
}

#[test]
fn p5_mcp_01_a_schema_may_hold_objects_only_as_an_arrays_items() {
    let read = |text: &str| Schema::read(&serde_json::from_str(text).unwrap());
    // As an array's items, an object is checked as the arguments are.
    let items = read(
        r#"{"type":"object","properties":{"xs":{"type":"array","items":{
            "type":"object",
            "properties":{"a":{"type":"integer"},"b":{"type":"string"}},
            "required":["a"],"additionalProperties":false
        }}},"additionalProperties":false}"#,
    )
    .unwrap();
    let args = |value: Value| value.as_object().unwrap().clone();
    assert!(validate(&items, &args(json!({"xs": [{"a": 1, "b": "x"}]}))).is_ok());
    let message = validate(&items, &args(json!({"xs": [{"b": "x"}]}))).unwrap_err();
    assert!(message.contains("an object with"), "{message}");
    assert!(validate(&items, &args(json!({"xs": [{"a": 1, "c": 1}]}))).is_err());
    assert!(validate(&items, &args(json!({"xs": [1]}))).is_err());
    // Anywhere else an object is refused at registration, as is anything a
    // property could not be checked with.
    assert!(
        read(
            r#"{"type":"object","properties":{"o":{"type":"object","properties":{},
                "additionalProperties":false}},"additionalProperties":false}"#
        )
        .unwrap_err()
        .contains("only as an array's items")
    );
    assert!(
        read(
            r#"{"type":"object","properties":{"xs":{"type":"array","items":{"type":"object",
                "properties":{"o":{"type":"array","items":{"type":"string"}}},
                "additionalProperties":false}}},"additionalProperties":false}"#
        )
        .unwrap_err()
        .contains("plain values")
    );
    assert!(
        read(
            r#"{"type":"object","properties":{"s":{"type":"string","minItems":1}},
                "additionalProperties":false}"#
        )
        .unwrap_err()
        .contains("minItems and maxItems are for arrays")
    );
    // An array's item count is checked.
    let counted = read(
        r#"{"type":"object","properties":{"xs":{"type":"array","minItems":1,"maxItems":2,
            "items":{"type":"string"}}},"additionalProperties":false}"#,
    )
    .unwrap();
    assert!(validate(&counted, &args(json!({"xs": ["a"]}))).is_ok());
    let message = validate(&counted, &args(json!({"xs": []}))).unwrap_err();
    assert!(message.contains("1 to 2 long"), "{message}");
    assert!(validate(&counted, &args(json!({"xs": ["a", "b", "c"]}))).is_err());
}

#[test]
fn p5_mcp_01_the_tool_says_it_may_delete_and_needs_a_person() {
    let mut registry = Registry::new(24_000, true);
    for tool in shipped() {
        registry.register(tool).unwrap();
    }
    let listed = serde_json::to_value(registry.list()).unwrap();
    let delete = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "duscope_delete")
        .unwrap();
    assert_eq!(delete["annotations"]["destructiveHint"], true);
    assert_eq!(delete["annotations"]["readOnlyHint"], false);
    assert_eq!(delete["annotations"]["idempotentHint"], false);
    assert_eq!(delete["annotations"]["openWorldHint"], false);
    assert_eq!(delete["_meta"]["anthropic/requiresUserInteraction"], true);
    // Its neighbours stay read-only, and the list stays sorted.
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.is_sorted(), "{names:?}");
    for tool in listed.as_array().unwrap() {
        if tool["name"] != "duscope_delete" && tool["name"] != "duscope_scan" {
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
            assert_eq!(tool["annotations"]["destructiveHint"], false, "{tool}");
        }
    }
}
