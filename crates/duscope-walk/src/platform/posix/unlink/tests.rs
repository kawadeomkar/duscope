//! P5-CORE-03: removing through descriptors. Every tree is the test's own,
//! under its temporary directory, beside an `outside` directory that a
//! removal must never touch; the races are made to happen at a chosen step
//! through [`hook::on_step`].

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;

#[cfg(target_os = "linux")]
use std::os::fd::AsFd;

use super::{CheckedDir, hook, remove_dir_checked, unlink_checked};
use crate::RemoveError;

/// `path`'s device and inode, as a plan records them.
fn ident(path: &Path) -> (u64, u64) {
    let meta = fs::symlink_metadata(path).unwrap();
    (meta.dev(), meta.ino())
}

/// A tree to remove, `root/item`, and `root/outside` beside it: files, a
/// directory three deep, an empty one, two links of one file both inside,
/// a file whose other link is outside, and a symbolic link to a file
/// outside and one to the outside directory.
fn tree(env: &TestEnv) -> PathBuf {
    tree_in(&fs::canonicalize(env.tmp_dir()).unwrap())
}

/// [`tree`] in the directory `dir`, as `dir/root`.
fn tree_in(dir: &Path) -> PathBuf {
    let root = dir.join("root");
    let item = root.join("item");
    let outside = root.join("outside");
    fs::create_dir_all(item.join("sub/deep")).unwrap();
    fs::create_dir_all(item.join("empty")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(item.join("a.bin"), vec![1u8; 9000]).unwrap();
    fs::write(item.join("sub/b.bin"), vec![2u8; 5000]).unwrap();
    fs::write(item.join("sub/deep/c.bin"), vec![3u8; 70_000]).unwrap();
    fs::write(item.join("h1.bin"), vec![4u8; 20_000]).unwrap();
    fs::hard_link(item.join("h1.bin"), item.join("sub/h2.bin")).unwrap();
    fs::write(item.join("kept.bin"), vec![5u8; 30_000]).unwrap();
    fs::hard_link(item.join("kept.bin"), outside.join("kept-link.bin")).unwrap();
    fs::write(outside.join("target.bin"), vec![6u8; 4000]).unwrap();
    symlink(outside.join("target.bin"), item.join("to-file")).unwrap();
    symlink(&outside, item.join("sub/to-dir")).unwrap();
    root
}

/// What removing `dir` and everything under it frees, as `du` counts it:
/// each directory, and each inode whose every link lies under `dir`.
fn freeable(dir: &Path) -> u64 {
    let mut links: HashMap<(u64, u64), (u64, u64, u64)> = HashMap::new();
    let mut total = 0;
    let mut stack = vec![dir.to_owned()];
    while let Some(path) = stack.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        let bytes = meta.blocks() * 512;
        if meta.is_dir() {
            total += bytes;
            for entry in fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
        } else {
            let seen = links
                .entry((meta.dev(), meta.ino()))
                .or_insert((meta.nlink(), bytes, 0));
            seen.2 += 1;
        }
    }
    total
        + links
            .values()
            .filter(|(nlink, _, inside)| inside == nlink)
            .map(|(_, bytes, _)| bytes)
            .sum::<u64>()
}

/// The outside directory holds what it held.
fn outside_intact(root: &Path) {
    let outside = root.join("outside");
    assert_eq!(fs::read(outside.join("target.bin")).unwrap(), [6u8; 4000]);
    assert_eq!(
        fs::read(outside.join("kept-link.bin")).unwrap(),
        vec![5u8; 30_000]
    );
}

#[test]
fn p5_core_03_a_directory_goes_bottom_up_and_frees_each_inode_once() {
    let env = TestEnv::new();
    let root = tree(&env);
    let item = root.join("item");
    let expected = freeable(&item);
    let parent = CheckedDir::open(&root).unwrap();
    assert_eq!(parent.path(), root);
    let mut freed = 0;
    remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed).unwrap();
    assert!(fs::symlink_metadata(&item).is_err());
    // The links were removed, not followed; kept.bin's bytes stay with its
    // link outside.
    outside_intact(&root);
    assert_eq!(
        fs::metadata(root.join("outside/kept-link.bin"))
            .unwrap()
            .nlink(),
        1
    );
    assert_eq!(freed, expected);
    assert!(freed > 0);
}

#[test]
fn p5_core_03_a_deep_tree_goes_without_recursion() {
    // Deeper than macOS's default limit of 256 descriptors.
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("root");
    let mut deepest = root.join("item");
    for _ in 0..300 {
        deepest.push("d");
    }
    fs::create_dir_all(&deepest).unwrap();
    fs::write(deepest.join("f.bin"), b"deep").unwrap();
    let item = root.join("item");
    let expected = freeable(&item);
    let parent = CheckedDir::open(&root).unwrap();
    let mut freed = 0;
    remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed).unwrap();
    assert!(fs::symlink_metadata(&item).is_err());
    assert_eq!(freed, expected);
}

#[test]
fn p5_core_03_an_entry_is_unlinked_only_when_it_is_the_expected_one() {
    let env = TestEnv::new();
    let root = tree(&env);
    let item = root.join("item");
    let dir = CheckedDir::open(&item).unwrap();
    let mut freed = 0;
    // A symbolic link goes itself; what it names stays.
    let link = ident(&item.join("to-file"));
    unlink_checked(&dir, OsStr::new("to-file"), link, &mut freed).unwrap();
    assert!(fs::symlink_metadata(item.join("to-file")).is_err());
    outside_intact(&root);
    // Another inode at the name: nothing goes.
    let (dev, ino) = ident(&item.join("a.bin"));
    let err = unlink_checked(&dir, OsStr::new("a.bin"), (dev, ino + 1), &mut freed).unwrap_err();
    assert!(
        matches!(&err, RemoveError::Changed { path } if *path == item.join("a.bin")),
        "{err:?}"
    );
    assert!(item.join("a.bin").exists());
    // A directory is not unlinked, nor a file removed as one.
    let sub = ident(&item.join("sub"));
    assert!(matches!(
        unlink_checked(&dir, OsStr::new("sub"), sub, &mut freed),
        Err(RemoveError::Changed { .. })
    ));
    assert!(matches!(
        remove_dir_checked(&dir, OsStr::new("a.bin"), (dev, ino), &mut freed),
        Err(RemoveError::Changed { .. })
    ));
    // The last link of a file frees its bytes; one of two frees nothing.
    let before = freed;
    let bytes = fs::symlink_metadata(item.join("h1.bin")).unwrap().blocks() * 512;
    let h1 = ident(&item.join("h1.bin"));
    unlink_checked(&dir, OsStr::new("h1.bin"), h1, &mut freed).unwrap();
    assert_eq!(freed, before);
    let sub_dir = CheckedDir::open(&item.join("sub")).unwrap();
    unlink_checked(&sub_dir, OsStr::new("h2.bin"), h1, &mut freed).unwrap();
    assert_eq!(freed, before + bytes);
    // A gone entry is a change, not a success.
    assert!(matches!(
        unlink_checked(&dir, OsStr::new("h1.bin"), h1, &mut freed),
        Err(RemoveError::Changed { .. })
    ));
}

#[test]
fn p5_core_03_another_device_and_inode_removes_nothing() {
    let env = TestEnv::new();
    let root = tree(&env);
    let item = root.join("item");
    let parent = CheckedDir::open(&root).unwrap();
    let (dev, ino) = ident(&item);
    let mut freed = 0;
    let err =
        remove_dir_checked(&parent, OsStr::new("item"), (dev, ino + 1), &mut freed).unwrap_err();
    assert!(matches!(err, RemoveError::Changed { .. }), "{err:?}");
    assert_eq!(freed, 0);
    assert!(item.join("sub/deep/c.bin").exists());
}

#[test]
fn p5_core_03_a_symbolic_link_on_the_way_is_never_followed() {
    let env = TestEnv::new();
    let root = tree(&env);
    // The outside directory through a link: refused, though what it names
    // is a directory.
    symlink(root.join("outside"), root.join("via")).unwrap();
    let err = CheckedDir::open(&root.join("via")).unwrap_err();
    assert!(
        matches!(&err, RemoveError::Changed { path } if *path == root.join("via")),
        "{err:?}"
    );
    assert!(matches!(
        CheckedDir::open(&root.join("item/a.bin")),
        Err(RemoveError::Changed { .. })
    ));
    assert!(matches!(
        CheckedDir::open(&root.join("gone")),
        Err(RemoveError::Changed { .. })
    ));
    // Only an absolute path, without `..`; only one name in the directory.
    for path in [PathBuf::from("root"), root.join("item/../item")] {
        assert!(
            matches!(CheckedDir::open(&path), Err(RemoveError::Io { .. })),
            "{}",
            path.display()
        );
    }
    let parent = CheckedDir::open(&root).unwrap();
    let mut freed = 0;
    for name in ["item/a.bin", "..", ".", ""] {
        assert!(
            matches!(
                unlink_checked(&parent, OsStr::new(name), (0, 0), &mut freed),
                Err(RemoveError::Io { .. })
            ),
            "{name:?}"
        );
    }
    outside_intact(&root);
}

/// Replaces the directory `path` with a symbolic link to `to`, keeping the
/// directory at `away`.
fn swap_for_link(path: &Path, away: &Path, to: &Path) {
    fs::rename(path, away).unwrap();
    symlink(to, path).unwrap();
}

#[test]
fn p5_core_03_a_directory_swapped_for_a_symlink_mid_removal_removes_nothing_outside() {
    let env = TestEnv::new();
    let root = tree(&env);
    let item = root.join("item");
    let outside = root.join("outside");
    // After the removal has read `sub` as a directory, before it opens it.
    let (sub, away, to) = (item.join("sub"), root.join("sub.away"), outside.clone());
    let _hook = hook::on_step(move |at| {
        if at == sub {
            swap_for_link(&sub, &away, &to);
        }
    });
    let parent = CheckedDir::open(&root).unwrap();
    let mut freed = 0;
    let err =
        remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed).unwrap_err();
    assert!(
        matches!(&err, RemoveError::Changed { path } if *path == item.join("sub")),
        "{err:?}"
    );
    outside_intact(&root);
    assert!(outside.join("target.bin").exists());
    // The directory moved away holds what it held.
    assert_eq!(fs::read(root.join("sub.away/b.bin")).unwrap(), [2u8; 5000]);
    assert_eq!(
        fs::read(root.join("sub.away/deep/c.bin")).unwrap(),
        vec![3u8; 70_000]
    );
}

#[test]
fn p5_core_03_the_directory_itself_swapped_after_its_check_removes_nothing() {
    let env = TestEnv::new();
    let root = tree(&env);
    let item = root.join("item");
    let expect = ident(&item);
    let (at_item, away, to) = (item.clone(), root.join("item.away"), root.join("outside"));
    let _hook = hook::on_step(move |at| {
        if at == at_item {
            swap_for_link(&at_item, &away, &to);
        }
    });
    let parent = CheckedDir::open(&root).unwrap();
    let mut freed = 0;
    let err = remove_dir_checked(&parent, OsStr::new("item"), expect, &mut freed).unwrap_err();
    assert!(matches!(err, RemoveError::Changed { .. }), "{err:?}");
    assert_eq!(freed, 0);
    outside_intact(&root);
    assert!(root.join("item.away/sub/deep/c.bin").exists());
    // The link left in its place is still a link.
    assert!(
        fs::symlink_metadata(&item)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn p5_core_03_a_file_swapped_for_a_symlink_mid_removal_unlinks_the_link() {
    let env = TestEnv::new();
    let root = tree(&env);
    let item = root.join("item");
    let (file, away, to) = (
        item.join("a.bin"),
        root.join("a.away"),
        root.join("outside/target.bin"),
    );
    let _hook = hook::on_step(move |at| {
        if at == file {
            fs::rename(&file, &away).unwrap();
            symlink(&to, &file).unwrap();
        }
    });
    let parent = CheckedDir::open(&root).unwrap();
    let mut freed = 0;
    remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed).unwrap();
    assert!(fs::symlink_metadata(&item).is_err());
    outside_intact(&root);
    assert_eq!(fs::read(root.join("a.away")).unwrap(), [1u8; 9000]);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts a tmpfs and a bind mount; root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_core_03_linux_a_mount_under_the_directory_is_never_crossed() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1")
        || crate::effective_uid() != 0
    {
        eprintln!("skipped: run as root with DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let base = fs::canonicalize(env.tmp_dir()).unwrap();
    // The kernel the tests run on, then each kernel that does not flag a
    // mount point: the descriptors' mounts tell it, else the mount table.
    let mut kernels = vec![hook::Kernel::default()];
    kernels.extend(old_kernels());
    for (i, kernel) in kernels.into_iter().enumerate() {
        let root = tree_in(&base.join(i.to_string()));
        let outside = root.join("outside");
        // A bind mount of a directory of the same device: st_dev cannot
        // tell it.
        let bound = root.join("bound");
        fs::create_dir_all(bound.join("bind")).unwrap();
        fs::write(bound.join("before.bin"), b"x").unwrap();
        mount(&env, &["--bind", path(&outside), path(&bound.join("bind"))]);
        let _unbind = Unmount(&env, bound.join("bind"));
        // A tmpfs: another device.
        let tmpfs = root.join("tmpfs");
        fs::create_dir_all(tmpfs.join("mnt")).unwrap();
        mount(&env, &["-t", "tmpfs", "tmpfs", path(&tmpfs.join("mnt"))]);
        let _unmount = Unmount(&env, tmpfs.join("mnt"));
        fs::write(tmpfs.join("mnt/on-tmpfs.bin"), vec![7u8; 1000]).unwrap();

        // The descriptors' mounts tell the bind mount apart.
        let (up, down) = (
            CheckedDir::open(&bound).unwrap(),
            CheckedDir::open(&bound.join("bind")).unwrap(),
        );
        assert_ne!(
            super::mount_id(up.fd.as_fd()).unwrap(),
            super::mount_id(down.fd.as_fd()).unwrap()
        );
        let _kernel = hook::as_kernel(kernel);
        let parent = CheckedDir::open(&root).unwrap();
        for (dir, mount_point) in [(&bound, bound.join("bind")), (&tmpfs, tmpfs.join("mnt"))] {
            let mut freed = 0;
            let name = dir.file_name().unwrap();
            let err = remove_dir_checked(&parent, name, ident(dir), &mut freed).unwrap_err();
            assert!(
                matches!(&err, RemoveError::Mount { path } if *path == mount_point),
                "{kernel:?}: {err:?}"
            );
            assert!(mount_point.is_dir());
        }
        outside_intact(&root);
        assert_eq!(
            fs::read(tmpfs.join("mnt/on-tmpfs.bin")).unwrap(),
            [7u8; 1000]
        );
        // Through the bind mount, the outside directory still holds its
        // files.
        assert!(bound.join("bind/target.bin").exists(), "{kernel:?}");
    }
    // Where nothing tells a mount point, neither directory is entered, and
    // nothing in either is removed.
    for (i, kernel) in nothing_tells().into_iter().enumerate() {
        let root = tree_in(&base.join(format!("nothing-{i}")));
        let bound = root.join("bound");
        fs::create_dir_all(bound.join("bind")).unwrap();
        fs::write(bound.join("before.bin"), b"x").unwrap();
        mount(
            &env,
            &[
                "--bind",
                path(&root.join("outside")),
                path(&bound.join("bind")),
            ],
        );
        let _unbind = Unmount(&env, bound.join("bind"));
        let _kernel = hook::as_kernel(kernel);
        let parent = CheckedDir::open(&root).unwrap();
        let mut freed = 0;
        let err = remove_dir_checked(&parent, OsStr::new("bound"), ident(&bound), &mut freed)
            .unwrap_err();
        assert!(
            matches!(&err, RemoveError::MountUnknown { path, .. } if *path == bound),
            "{kernel:?}: {err:?}"
        );
        assert_eq!(freed, 0);
        assert!(bound.join("before.bin").exists());
        outside_intact(&root);
    }
}

/// The kernels that do not flag a mount point, each with an answer left:
/// before 5.8 (no mount-root attribute) and without `statx`, each with
/// file handles, then with the mount table alone.
#[cfg(target_os = "linux")]
fn old_kernels() -> Vec<hook::Kernel> {
    let mut kernels = Vec::new();
    for no_handles in [false, true] {
        for (no_mount_root, no_statx) in [(true, false), (false, true)] {
            kernels.push(hook::Kernel {
                no_mount_root,
                no_statx,
                no_handles,
                no_mount_table: false,
            });
        }
    }
    kernels
}

/// The kernels that do not flag a mount point, without file handles or a
/// mount table.
#[cfg(target_os = "linux")]
fn nothing_tells() -> Vec<hook::Kernel> {
    old_kernels()
        .into_iter()
        .filter(|k| k.no_handles)
        .map(|k| hook::Kernel {
            no_mount_table: true,
            ..k
        })
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn p5_core_03_linux_a_descriptors_mount_id_is_the_one_the_mount_table_lists() {
    let env = TestEnv::new();
    let root = tree(&env);
    let sub = CheckedDir::open(&root.join("item/sub")).unwrap();
    match super::mount_id(sub.fd.as_fd()) {
        Ok(id) => assert_eq!(
            id,
            crate::platform::linux::Mounts::default().mnt_id_of(&root)
        ),
        // A temporary directory on a filesystem without file handles.
        Err(e) if e.raw_os_error() == Some(libc::EOPNOTSUPP) => {
            eprintln!("skipped: no file handles under {}", root.display());
        }
        Err(e) => panic!("name_to_handle_at: {e}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn p5_core_03_linux_a_kernel_that_flags_no_mount_point_still_removes_a_plain_tree() {
    // The descriptors' mounts, or the mount table, find no mount point, and
    // the tree goes as it does where statx flags one.
    let env = TestEnv::new();
    let base = fs::canonicalize(env.tmp_dir()).unwrap();
    for (i, kernel) in old_kernels().into_iter().enumerate() {
        let root = tree_in(&base.join(i.to_string()));
        let item = root.join("item");
        let expected = freeable(&item);
        let _kernel = hook::as_kernel(kernel);
        let parent = CheckedDir::open(&root).unwrap();
        let mut freed = 0;
        remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed)
            .unwrap_or_else(|e| panic!("{kernel:?}: {e:?}"));
        assert!(fs::symlink_metadata(&item).is_err(), "{kernel:?}");
        assert_eq!(freed, expected, "{kernel:?}");
        outside_intact(&root);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn p5_core_03_linux_where_nothing_tells_a_mount_point_nothing_is_removed() {
    let env = TestEnv::new();
    let base = fs::canonicalize(env.tmp_dir()).unwrap();
    for (i, kernel) in nothing_tells().into_iter().enumerate() {
        let root = tree_in(&base.join(i.to_string()));
        let item = root.join("item");
        let _kernel = hook::as_kernel(kernel);
        let parent = CheckedDir::open(&root).unwrap();
        let mut freed = 0;
        let err =
            remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed).unwrap_err();
        assert!(
            matches!(&err, RemoveError::MountUnknown { path, .. } if *path == item),
            "{kernel:?}: {err:?}"
        );
        // Refused before anything in it went.
        assert_eq!(freed, 0);
        assert!(item.join("a.bin").exists() && item.join("sub/deep/c.bin").exists());
        // A file needs no mount check.
        let dir = CheckedDir::open(&item).unwrap();
        unlink_checked(
            &dir,
            OsStr::new("a.bin"),
            ident(&item.join("a.bin")),
            &mut freed,
        )
        .unwrap();
    }
    // With the mount-root attribute (Linux 5.8 on), neither file handles nor
    // the mount table is asked: the tree goes without them.
    let kernel = hook::Kernel {
        no_handles: true,
        no_mount_table: true,
        ..hook::Kernel::default()
    };
    let root = tree_in(&base.join("flagged"));
    let item = root.join("item");
    let parent = CheckedDir::open(&root).unwrap();
    let flagged = super::stat_at(parent.fd.as_fd(), c"item")
        .unwrap()
        .mount
        .is_some();
    if !flagged {
        eprintln!("this kernel's statx flags no mount root (before 5.8): the last part skipped");
        return;
    }
    let _kernel = hook::as_kernel(kernel);
    let mut freed = 0;
    remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed).unwrap();
    assert!(fs::symlink_metadata(&item).is_err());
}

/// P5-CORE-10: what `clean`'s plan asks of an entry, each kernel answering
/// by what it has: a file and a directory of the test's tree are no mount's
/// root, `/proc` is one, and where nothing answers nothing is said.
#[cfg(target_os = "linux")]
#[test]
fn p5_core_10_linux_an_entry_is_a_mount_root_by_whatever_the_kernel_tells() {
    let env = TestEnv::new();
    let root = tree(&env);
    let item = CheckedDir::open(&root.join("item")).unwrap();
    let slash = CheckedDir::open(Path::new("/")).unwrap();
    let mut kernels = vec![hook::Kernel::default()];
    kernels.extend(old_kernels());
    for kernel in kernels {
        let _kernel = hook::as_kernel(kernel);
        for name in ["a.bin", "sub", "empty"] {
            assert_eq!(
                item.mount_root(OsStr::new(name)),
                Some(false),
                "{kernel:?}: {name}"
            );
        }
        assert_eq!(
            slash.mount_root(OsStr::new("proc")),
            Some(true),
            "{kernel:?}"
        );
    }
    for kernel in nothing_tells() {
        let _kernel = hook::as_kernel(kernel);
        assert_eq!(item.mount_root(OsStr::new("a.bin")), None, "{kernel:?}");
        assert_eq!(slash.mount_root(OsStr::new("proc")), None, "{kernel:?}");
    }
    // An entry that is not there says nothing either.
    assert_eq!(item.mount_root(OsStr::new("gone")), None);
}

#[cfg(target_os = "linux")]
fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// Runs `mount` with `args`.
#[cfg(target_os = "linux")]
fn mount(env: &TestEnv, args: &[&str]) {
    let out = env.command("mount").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "mount {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Unmounts its path when dropped.
#[cfg(target_os = "linux")]
struct Unmount<'a>(&'a TestEnv, PathBuf);

#[cfg(target_os = "linux")]
impl Drop for Unmount<'_> {
    fn drop(&mut self) {
        let _ = self.0.command("umount").arg(&self.1).output();
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p5_core_03_macos_a_mounted_image_under_the_directory_is_never_crossed() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let root = tree(&env);
    let mount = root.join("item/sub/mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = root.join("image.dmg");
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
    env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
    let _detach = Detach(&env, mount.clone());
    fs::write(mount.join("on-image.bin"), vec![7u8; 1000]).unwrap();
    let item = root.join("item");
    let parent = CheckedDir::open(&root).unwrap();
    let mut freed = 0;
    let err =
        remove_dir_checked(&parent, OsStr::new("item"), ident(&item), &mut freed).unwrap_err();
    assert!(
        matches!(&err, RemoveError::Mount { path } if *path == mount),
        "{err:?}"
    );
    assert_eq!(fs::read(mount.join("on-image.bin")).unwrap(), [7u8; 1000]);
    outside_intact(&root);
}

/// Detaches the disk image mounted at its path when dropped.
#[cfg(target_os = "macos")]
struct Detach<'a>(&'a TestEnv, PathBuf);

#[cfg(target_os = "macos")]
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

/// P5-CORE-05: the Trash's move on Linux, through descriptors, into a
/// `files` directory of the test's own beside the tree, never a real Trash.
#[cfg(target_os = "linux")]
mod rename {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::os::fd::AsFd;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use duscope_testkit::TestEnv;

    use super::{ident, outside_intact, swap_for_link, tree};
    use crate::RemoveError;
    use crate::platform::posix::system::effective_uid;
    use crate::platform::posix::unlink::{
        CheckedDir, RenameError, hook, remove_dir_checked, rename_checked, unlink_checked,
    };

    /// The tree's own `trash/files`, open as a directory.
    fn files(root: &Path) -> fs::File {
        let dir = root.join("trash/files");
        fs::create_dir_all(&dir).unwrap();
        fs::File::open(dir).unwrap()
    }

    /// The names in the directory `dir`, sorted.
    fn names(dir: &Path) -> Vec<OsString> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    /// Runs `test` with `renameat2` taking `RENAME_NOREPLACE`, then with it
    /// refused as NFS or a FUSE server without `RENAME2` refuses it, each
    /// on a tree of its own.
    fn both_ways(test: impl Fn(&Path)) {
        for refused in [false, true] {
            let env = TestEnv::new();
            let root = tree(&env);
            let _refused = refused.then(hook::without_noreplace);
            test(&root);
        }
    }

    /// Gives each directory its mode back when dropped, so that the test's
    /// temporary directory can be removed however the test ends.
    struct Modes(Vec<(PathBuf, u32)>);

    impl Drop for Modes {
        fn drop(&mut self) {
            for (dir, mode) in &self.0 {
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(*mode));
            }
        }
    }

    #[test]
    fn p5_core_05_linux_a_directory_that_can_be_searched_but_not_read_is_passed_through() {
        // Root is held to no directory's mode: there the test shows nothing.
        if effective_uid() == 0 {
            eprintln!("skipped: root reads a directory whatever its mode");
            return;
        }
        let env = TestEnv::new();
        let root = tree(&env);
        // On the way, a directory that may only be searched (0111); in it,
        // the entries' own directory, which may be searched and written but
        // not read (0311). A lookup by path passes both, and so must the
        // descriptors: a move to the Trash, a deletion of a file and one of
        // a directory with what is under it.
        let locked = root.join("locked");
        let parent = locked.join("parent");
        fs::create_dir_all(parent.join("dir/sub")).unwrap();
        fs::write(parent.join("a.bin"), b"to the Trash").unwrap();
        fs::write(parent.join("b.bin"), b"deleted").unwrap();
        fs::write(parent.join("dir/sub/c.bin"), vec![3u8; 9000]).unwrap();
        let files = files(&root);
        let (a, b, dir) = (
            ident(&parent.join("a.bin")),
            ident(&parent.join("b.bin")),
            ident(&parent.join("dir")),
        );
        let _modes = Modes(vec![(locked.clone(), 0o755), (parent.clone(), 0o755)]);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o311)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o111)).unwrap();
        assert!(fs::read_dir(&locked).is_err() && fs::read_dir(&parent).is_err());
        let checked = CheckedDir::open(&parent).unwrap();
        rename_checked(
            &checked,
            OsStr::new("a.bin"),
            a,
            files.as_fd(),
            OsStr::new("a.bin"),
        )
        .unwrap();
        let mut freed = 0;
        unlink_checked(&checked, OsStr::new("b.bin"), b, &mut freed).unwrap();
        remove_dir_checked(&checked, OsStr::new("dir"), dir, &mut freed).unwrap();
        assert!(freed > 0);
        assert_eq!(
            fs::read(root.join("trash/files/a.bin")).unwrap(),
            b"to the Trash"
        );
        for name in ["a.bin", "b.bin", "dir"] {
            assert!(fs::symlink_metadata(parent.join(name)).is_err(), "{name}");
        }
    }

    #[test]
    fn p5_core_05_linux_a_parent_swapped_for_a_symlink_moves_nothing() {
        both_ways(|root| {
            let item = root.join("item");
            let outside = root.join("outside");
            // Outside, a link of the very file the plan saw: through a
            // symbolic link in place of its directory the path leads to the
            // same device and inode, so only the way there tells them apart.
            fs::hard_link(item.join("a.bin"), outside.join("a.bin")).unwrap();
            let expect = ident(&item.join("a.bin"));
            let files = files(root);
            // As the way to the entry is opened: its directory goes, and a
            // link to the outside directory takes its place.
            let (at, away, to) = (item.clone(), root.join("item.away"), outside.clone());
            let hook = hook::on_step(move |path| {
                if path == at {
                    swap_for_link(&at, &away, &to);
                }
            });
            let err = CheckedDir::open(&item).unwrap_err();
            assert!(
                matches!(&err, RemoveError::Changed { path } if *path == item),
                "{err:?}"
            );
            drop(hook);
            // Nor does a rename through the link's directory take the file
            // outside: the parent is opened afresh, and refused again.
            assert!(matches!(
                CheckedDir::open(&item),
                Err(RemoveError::Changed { .. })
            ));
            // The link outside stays where it was, and the directory the
            // swap moved holds the file the plan saw: nothing moved.
            assert!(names(&root.join("trash/files")).is_empty());
            assert_eq!(fs::read(outside.join("a.bin")).unwrap(), [1u8; 9000]);
            assert_eq!(fs::read(root.join("item.away/a.bin")).unwrap(), [1u8; 9000]);
            outside_intact(root);
            // An entry swapped for a symbolic link to it is another entry:
            // the link is not moved, and the file it names stays.
            let parent = CheckedDir::open(&root.join("item.away")).unwrap();
            fs::rename(root.join("item.away/a.bin"), root.join("a.away")).unwrap();
            symlink(outside.join("a.bin"), root.join("item.away/a.bin")).unwrap();
            let err = rename_checked(
                &parent,
                OsStr::new("a.bin"),
                expect,
                files.as_fd(),
                OsStr::new("a.bin"),
            )
            .unwrap_err();
            assert!(
                matches!(err, RenameError::Entry(RemoveError::Changed { .. })),
                "{err:?}"
            );
            assert!(names(&root.join("trash/files")).is_empty());
            assert_eq!(fs::read(outside.join("a.bin")).unwrap(), [1u8; 9000]);
        });
    }

    #[test]
    fn p5_core_05_linux_a_parent_swapped_after_it_is_opened_moves_only_the_planned_entry() {
        both_ways(|root| {
            let item = root.join("item");
            let outside = root.join("outside");
            fs::write(outside.join("a.bin"), b"outside").unwrap();
            let expect = ident(&item.join("a.bin"));
            let files = files(root);
            let parent = CheckedDir::open(&item).unwrap();
            // Once its directory is open, just before the rename: the
            // directory goes, a link to the outside one in its place.
            let (at, dir, away, to) = (
                item.join("a.bin"),
                item.clone(),
                root.join("item.away"),
                outside.clone(),
            );
            let _hook = hook::on_step(move |path| {
                if path == at {
                    swap_for_link(&dir, &away, &to);
                }
            });
            rename_checked(
                &parent,
                OsStr::new("a.bin"),
                expect,
                files.as_fd(),
                OsStr::new("a.bin"),
            )
            .unwrap();
            // The rename went through the directory's descriptor: the
            // planned file moved, from where its directory now is, and the
            // file of the same name outside stayed.
            assert_eq!(
                fs::read(root.join("trash/files/a.bin")).unwrap(),
                [1u8; 9000]
            );
            assert!(fs::symlink_metadata(root.join("item.away/a.bin")).is_err());
            assert_eq!(fs::read(outside.join("a.bin")).unwrap(), b"outside");
            outside_intact(root);
        });
    }

    #[test]
    fn p5_core_05_linux_an_entry_swapped_before_the_rename_is_moved_back() {
        both_ways(|root| {
            let item = root.join("item");
            let file = item.join("a.bin");
            let expect = ident(&file);
            let files = files(root);
            // After the check, just before the rename: the file goes, and
            // another takes its name.
            let (at, away) = (file.clone(), root.join("a.away"));
            let _hook = hook::on_step(move |path| {
                if path == at {
                    fs::rename(&at, &away).unwrap();
                    fs::write(&at, b"swapped in").unwrap();
                }
            });
            let parent = CheckedDir::open(&item).unwrap();
            let err = rename_checked(
                &parent,
                OsStr::new("a.bin"),
                expect,
                files.as_fd(),
                OsStr::new("a.bin"),
            )
            .unwrap_err();
            assert!(
                matches!(&err, RenameError::Entry(RemoveError::Changed { path }) if *path == file),
                "{err:?}"
            );
            // What was moved is back at its name, the Trash is empty, and
            // the file the plan saw is where the swap put it.
            assert_eq!(fs::read(&file).unwrap(), b"swapped in");
            assert!(names(&root.join("trash/files")).is_empty());
            assert_eq!(fs::read(root.join("a.away")).unwrap(), [1u8; 9000]);
        });
    }

    #[test]
    fn p5_core_05_linux_a_swapped_entry_whose_name_is_taken_again_stays_in_the_trash() {
        both_ways(|root| {
            let item = root.join("item");
            let file = item.join("a.bin");
            let expect = ident(&file);
            let files = files(root);
            // Another file at the name before the rename, and a third once
            // the second has been moved: the second cannot go back.
            let (at, away) = (file.clone(), root.join("a.away"));
            let _hook = hook::on_step(move |path| {
                if path == at {
                    fs::rename(&at, &away).unwrap();
                    fs::write(&at, b"second").unwrap();
                }
            });
            let parent = CheckedDir::open(&item).unwrap();
            let taken = item.join("a.bin");
            let _again = hook::on_moved(move || fs::write(&taken, b"third").unwrap());
            let err = rename_checked(
                &parent,
                OsStr::new("a.bin"),
                expect,
                files.as_fd(),
                OsStr::new("a.bin"),
            )
            .unwrap_err();
            assert!(
                matches!(&err, RenameError::Stranded(e) if e.raw_os_error() == Some(libc::EEXIST)),
                "{err:?}"
            );
            // Nothing was replaced: the third file holds the name, the
            // second waits in the Trash, the first is where the swap put it.
            assert_eq!(fs::read(&file).unwrap(), b"third");
            assert_eq!(fs::read(root.join("trash/files/a.bin")).unwrap(), b"second");
            assert_eq!(fs::read(root.join("a.away")).unwrap(), [1u8; 9000]);
        });
    }

    #[test]
    fn p5_core_05_linux_a_taken_name_is_never_replaced() {
        both_ways(|root| {
            let item = root.join("item");
            let files = files(root);
            fs::write(root.join("trash/files/a.bin"), b"trashed before").unwrap();
            fs::create_dir(root.join("trash/files/sub")).unwrap();
            let parent = CheckedDir::open(&item).unwrap();
            for (name, kind) in [("a.bin", "file"), ("sub", "directory")] {
                let expect = ident(&item.join(name));
                let err = rename_checked(
                    &parent,
                    OsStr::new(name),
                    expect,
                    files.as_fd(),
                    OsStr::new(name),
                )
                .unwrap_err();
                assert!(matches!(err, RenameError::Taken), "{kind}: {err:?}");
                assert_eq!(ident(&item.join(name)), expect, "{kind}");
            }
            assert_eq!(
                fs::read(root.join("trash/files/a.bin")).unwrap(),
                b"trashed before"
            );
            assert!(names(&root.join("trash/files/sub")).is_empty());
            // Under a free name each moves whole: a file, a directory with
            // what is under it, and a symbolic link itself, never what it
            // names.
            for name in ["a.bin", "sub", "to-file"] {
                let expect = ident(&item.join(name));
                let moved = format!("{name}.2");
                rename_checked(
                    &parent,
                    OsStr::new(name),
                    expect,
                    files.as_fd(),
                    OsStr::new(&moved),
                )
                .unwrap();
                assert!(fs::symlink_metadata(item.join(name)).is_err(), "{name}");
                assert_eq!(ident(&root.join("trash/files").join(&moved)), expect);
            }
            assert_eq!(
                fs::read(root.join("trash/files/sub.2/deep/c.bin")).unwrap(),
                vec![3u8; 70_000]
            );
            outside_intact(root);
            // A name that is not one name in its directory reaches nowhere.
            let expect = ident(&item.join("kept.bin"));
            for target in ["../kept.bin", "..", ".", ""] {
                assert!(
                    matches!(
                        rename_checked(
                            &parent,
                            OsStr::new("kept.bin"),
                            expect,
                            files.as_fd(),
                            OsStr::new(target),
                        ),
                        Err(RenameError::Io(_))
                    ),
                    "{target:?}"
                );
            }
            assert_eq!(ident(&item.join("kept.bin")), expect);
        });
    }
}
