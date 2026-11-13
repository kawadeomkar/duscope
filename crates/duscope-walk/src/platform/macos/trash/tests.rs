//! P5-CORE-06: a refused move to the Trash is told apart by its `NSError`'s
//! domain and code. The platform test reads the codes `NSFileManager`
//! answers on a disk image it attaches; nothing on the startup volume is ever
//! handed to it.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use objc2_foundation::{NSCocoaErrorDomain, NSInteger};

use super::{COCOA_ERROR_DOMAIN, classify, move_item, trash, url_text};
use crate::TrashError;

/// Whether [`classify`] calls a refusal of `domain` and `code`
/// `Unavailable`; its message is passed on either way.
fn unavailable(domain: &str, code: NSInteger) -> bool {
    let message = format!("{domain} {code}");
    let (unavailable, said) = match classify(domain, code, message.clone()) {
        TrashError::Unavailable(said) => (true, said),
        TrashError::Failed(said) => (false, said),
        changed @ TrashError::Changed { .. } => panic!("not a refusal: {changed:?}"),
    };
    assert_eq!(said, message);
    unavailable
}

#[test]
fn p5_core_06_only_a_volume_without_a_trash_is_unavailable() {
    // NSFeatureUnsupportedError: the volume has no Trash.
    assert!(unavailable(COCOA_ERROR_DOMAIN, 3328));
    // No permission to write (a locked file too) or to read; no such file; a
    // Trash that cannot be made; a read-only volume; anything else, and 3328
    // of another domain.
    for code in [513, 257, 4, 512, 642, 0, -1, 3327, 3329] {
        assert!(!unavailable(COCOA_ERROR_DOMAIN, code), "{code}");
    }
    for domain in ["NSPOSIXErrorDomain", "NSOSStatusErrorDomain", ""] {
        assert!(!unavailable(domain, 3328), "{domain}");
    }
}

#[test]
fn p5_core_06_the_domain_is_foundations_own() {
    // SAFETY: `NSCocoaErrorDomain` is an immutable `NSString` constant that
    // Foundation exports and never changes; reading it has no other effect.
    let domain = unsafe { NSCocoaErrorDomain };
    assert_eq!(domain.to_string(), COCOA_ERROR_DOMAIN);
}

#[test]
fn p5_core_06_a_path_that_ends_in_no_name_is_never_handed_on() {
    for path in ["", "/", "/a/..", "a/..", ".."] {
        assert_eq!(url_text(Path::new(path)), None, "{path:?}");
    }
    let not_utf8 = Path::new(OsStr::from_bytes(b"/a/\xff.bin"));
    assert_eq!(url_text(not_utf8), None);
    for path in ["/a/b.bin", "/a/b/", "/a/./b", "b.bin", "/a/Café.bin"] {
        assert_eq!(url_text(Path::new(path)), Some(path), "{path:?}");
    }
}

#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p5_core_06_macos_an_image_answers_by_code() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("image");
    let mount = work.join("mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = work.join("image.dmg");
    env.hdiutil(&[
        &"create",
        &"-ov",
        &"-size",
        &"16m",
        &"-fs",
        &"APFS",
        &"-volname",
        &"DUSCOPE",
        &image,
    ]);
    let uid = fs::metadata(&work).unwrap().uid().to_string();
    let kept = mount.join("kept.bin");
    {
        env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
        let _detach = Detach(&env, mount.clone());
        // `move_item` has no guard of its own: nothing is handed to it
        // unless the image is mounted.
        assert_ne!(dev(&mount), dev(&work));
        // Through the whole move: a file goes to the volume's Trash, and a
        // locked one, in a folder this user may write to, is refused by
        // `NSFileManager` for want of permission.
        let moved = mount.join("moved.bin");
        fs::write(&moved, [1u8; 100]).unwrap();
        trash(&moved, ident(&moved), true).unwrap();
        assert!(!moved.exists());
        let in_trash = mount.join(".Trashes").join(&uid).join("moved.bin");
        assert_eq!(fs::read(in_trash).unwrap(), [1u8; 100]);
        let locked = mount.join("locked.bin");
        fs::write(&locked, [2u8; 100]).unwrap();
        chflags(&env, "uchg", &locked);
        let refused = failed(trash(&locked, ident(&locked), true));
        assert!(refused.contains("(NSCocoaErrorDomain 513)"), "{refused}");
        assert_eq!(fs::read(&locked).unwrap(), [2u8; 100]);
        chflags(&env, "nouchg", &locked);
        // Past the checks before the call: a folder locked too, which no
        // one may change (root included, as CI runs this), and an entry
        // that is not there.
        let shut = mount.join("shut");
        fs::create_dir(&shut).unwrap();
        fs::write(shut.join("a.bin"), [3u8; 100]).unwrap();
        chflags(&env, "uchg", &shut);
        let refused = failed(move_item(&shut.join("a.bin")));
        assert!(refused.contains("(NSCocoaErrorDomain 513)"), "{refused}");
        assert_eq!(fs::read(shut.join("a.bin")).unwrap(), [3u8; 100]);
        chflags(&env, "nouchg", &shut);
        let refused = failed(move_item(&mount.join("missing.bin")));
        assert!(refused.contains("(NSCocoaErrorDomain 4)"), "{refused}");
        fs::write(&kept, [4u8; 100]).unwrap();
        env.hdiutil(&[&"detach", &mount]);
    }
    // Read-only, the volume has no Trash: the one refusal that is
    // `Unavailable`. (`trash` itself refuses earlier, as the folder is not
    // writable; `NSFileManager` is asked here directly.)
    env.hdiutil(&[
        &"attach",
        &"-readonly",
        &"-nobrowse",
        &"-mountpoint",
        &mount,
        &image,
    ]);
    let detach = Detach(&env, mount.clone());
    assert_ne!(dev(&mount), dev(&work));
    match move_item(&kept) {
        Err(TrashError::Unavailable(message)) => {
            assert!(message.contains("(NSCocoaErrorDomain 3328)"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&kept).unwrap(), [4u8; 100]);
    drop(detach);
}

/// The device `path` is on.
fn dev(path: &Path) -> u64 {
    fs::metadata(path).unwrap().dev()
}

/// The device and inode of `path` itself, as a plan records them.
fn ident(path: &Path) -> (u64, u64) {
    let meta = fs::symlink_metadata(path).unwrap();
    (meta.dev(), meta.ino())
}

/// The message of a [`TrashError::Failed`].
fn failed(result: Result<(), TrashError>) -> String {
    match result {
        Err(TrashError::Failed(message)) => message,
        other => panic!("{other:?}"),
    }
}

/// Sets or clears a file flag on `path` with `chflags(1)`.
fn chflags(env: &TestEnv, flag: &str, path: &Path) {
    let out = env.command("chflags").arg(flag).arg(path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Detaches the disk image mounted at its path when dropped.
struct Detach<'a>(&'a TestEnv, PathBuf);

impl Drop for Detach<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .command("hdiutil")
            .args(["detach", "-force"])
            .arg(&self.1)
            .output();
    }
}
