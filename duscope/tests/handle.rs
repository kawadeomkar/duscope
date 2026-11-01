//! P2-CORE-01: `explain --json` returns a handle, signed with the key it
//! makes in the index directory, that names the path as `explain` found it
//! and holds for fifteen minutes; a key others may read issues none.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, Permissions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Output;

use duscope_core::CoreError;
use duscope_core::handle::{Handle, KEY_FILE, Key, LIFETIME_NS};
use duscope_core::query::timegrammar;
use duscope_testkit::TestEnv;
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn json(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn p2_core_01_explain_hands_out_a_handle_that_verifies() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("tree");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/big.bin"), vec![b'x'; 100_000]).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let scan = json(&run(
        &env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
    ));
    let file = root.join("sub/big.bin");
    let explained = json(&run(&env, &["explain", file.to_str().unwrap(), "--json"]));
    let token = explained["result"]["handle"].as_str().unwrap();
    // The key the command made, 0600, beside the indexes.
    let key_file = env.index_dir().join(KEY_FILE);
    assert_eq!(
        fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let key = Key::from_bytes(fs::read(&key_file).unwrap().try_into().unwrap());
    let now = timegrammar::parse_date(env.test_now()).unwrap().ns;
    let payload = Handle::verify(&key, token, now).unwrap();
    let found = fs::symlink_metadata(&file).unwrap();
    assert_eq!(payload.path, file.as_os_str().as_bytes());
    assert_eq!((payload.dev, payload.ino), (found.dev(), found.ino()));
    assert_eq!(
        payload.allocated,
        explained["result"]["allocated_bytes"].as_u64().unwrap()
    );
    assert_eq!(payload.scan_id, scan["scan"]["id"].as_str().unwrap());
    assert_eq!(payload.expires_at, now + LIFETIME_NS);
    assert!(matches!(
        Handle::verify(&key, token, now + LIFETIME_NS + 1),
        Err(CoreError::HandleExpired)
    ));
    // A path no saved scan holds gets a handle too, with no scan.
    let elsewhere = env.tmp_dir().join("loose.txt");
    fs::write(&elsewhere, b"x").unwrap();
    let explained = json(&run(
        &env,
        &["explain", elsewhere.to_str().unwrap(), "--json"],
    ));
    let token = explained["result"]["handle"].as_str().unwrap();
    assert_eq!(Handle::verify(&key, token, now).unwrap().scan_id, "");
    // A key others may read issues no handle; explain answers still.
    fs::set_permissions(&key_file, Permissions::from_mode(0o644)).unwrap();
    let explained = json(&run(&env, &["explain", file.to_str().unwrap(), "--json"]));
    assert_eq!(explained["result"]["handle"], Value::Null);
}
