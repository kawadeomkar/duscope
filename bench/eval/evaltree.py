"""The tree the eval's tasks run on: a small home directory, built the same
way every time.

Every name, size and content byte is fixed here; every modification time is
an age before the moment of the build, so "not modified for a year" means
the same files whichever day the eval runs. The access time equals the
modification time. Content is a pseudorandom block, so no filesystem
compresses it and allocated sizes follow the lengths.

The tree is built fresh for each run, so nothing an agent did in one run is
seen by the next. Every change time (ctime) is the build's, which `duscope
stale` also requires to be old: on this tree it lists nothing, and the
"unused for a year" questions are about modification time (tasks.toml).

`build(root, now, scale)` writes it; `grow(root, now, scale)` makes the
changes the diff task's second scan sees; `manifest(root)` is what a run must
leave unchanged. `scale` (below 1 for the harness's own tests) multiplies
every size.
"""

from __future__ import annotations

import hashlib
import os
from typing import Dict, Iterator, List, Optional, Tuple

K = 1024
M = 1024 * K
DAY = 86400

# (path under the root, size in bytes, age in days). Sizes chosen so that each
# task's answer stands clear of the next candidate (bench/test_eval.py checks
# the margins): over 10 MiB only the files below that say so.
FILES: List[Tuple[str, int, float]] = [
    # Movies: the largest file, a recent large one, a stale one under 10 MiB.
    ("Movies/holiday-2025.mov", 16 * M, 500),
    ("Movies/screen-recording.mov", 11 * M, 20),
    ("Movies/lecture-01.mkv", 9 * M, 730),
    ("Movies/clips/clip-a.mov", 3 * M, 10),
    ("Movies/clips/clip-b.mov", 1 * M, 11),
    ("Movies/.DS_Store", 6 * K, 2),
    # Downloads: the largest directory; old installers and archives.
    ("Downloads/ubuntu-24.04.1-live-server-arm64.iso", 15 * M, 800),
    ("Downloads/Xcode_16.1.xip", 14 * M, 730),
    ("Downloads/photo-dump.tar.gz", 12 * M, 400),
    ("Downloads/design-assets.zip", 11 * M, 100),
    ("Downloads/vlc-3.0.20.dmg", 6 * M, 500),
    ("Downloads/old-backup.tar.gz", 3 * M, 900),
    ("Downloads/setup.dmg", 2 * M, 3),
    ("Downloads/invoice-2026-10.pdf", 180 * K, 40),
    ("Downloads/notes.txt", 2 * K, 0.1),
    # Backups: an old archive, the second copy of a large name, a .tar.zst.
    ("Backups/2023/home-2023.tar.gz", 13 * M, 700),
    ("Backups/2024/ml/dataset.parquet", 11 * M + 512 * K, 300),
    ("Backups/phone-2022.tar.zst", 9 * M, 1000),
    ("Backups/phone/IMG_2041.heic", 4 * M, 600),
    # Documents: a recent .tar.gz among the documents.
    ("Documents/exports/report-2025.tar.gz", 5 * M, 30),
    ("Documents/thesis/thesis-final.pdf", 4 * M, 60),
    ("Documents/budget.xlsx", 80 * K, 15),
    # Pictures: a second, smaller file of a shared name.
    ("Pictures/trip/IMG_2041.heic", 4 * M, 60),
    # Projects: a web app, a Rust command and a Python notebook, each with
    # the caches its tools regenerate, each over 10 MiB so that `duscope
    # caches`, whose --min-size is 10 MiB by default, lists all three.
    ("Projects/web-app/package.json", 900, 60),
    ("Projects/web-app/package-lock.json", 60 * K, 60),
    ("Projects/web-app/src/index.ts", 3 * K, 5),
    ("Projects/web-app/src/app.tsx", 8 * K, 5),
    ("Projects/web-app/node_modules/typescript/lib/typescript.js", 7 * M, 60),
    ("Projects/web-app/node_modules/esbuild/bin/esbuild", 3 * M, 60),
    ("Projects/rust-cli/Cargo.toml", 400, 40),
    ("Projects/rust-cli/Cargo.lock", 24 * K, 40),
    ("Projects/rust-cli/src/main.rs", 6 * K, 4),
    ("Projects/rust-cli/assets/demo.gif", 2 * M, 40),
    ("Projects/rust-cli/target/debug/rust-cli", 11 * M, 2),
    ("Projects/rust-cli/target/debug/deps/librust_cli-0123456789abcdef.rlib", 1536 * K, 2),
    ("Projects/rust-cli/target/release/rust-cli", 2 * M, 9),
    ("Projects/ml-notebook/pyproject.toml", 800, 90),
    ("Projects/ml-notebook/src/model/__init__.py", 2 * K, 30),
    ("Projects/ml-notebook/src/model/__pycache__/__init__.cpython-312.pyc", 3 * K, 30),
    ("Projects/ml-notebook/.venv/pyvenv.cfg", 200, 90),
    ("Projects/ml-notebook/.venv/bin/python3.12", 50 * K, 90),
    (
        "Projects/ml-notebook/.venv/lib/python3.12/site-packages/numpy/_core/"
        "_multiarray_umath.cpython-312-darwin.so",
        9 * M + 512 * K,
        90,
    ),
    ("Projects/ml-notebook/data/dataset.parquet", 11 * M + 512 * K, 200),
    # Dotfiles.
    (".bashrc", 3 * K, 700),
    (".zshrc", 2 * K, 50),
    (".gitconfig", 300, 365 * 2),
    (".ssh/config", 400, 300),
    (".config/editor/settings.json", 2 * K, 20),
]

# The cargo target directory's tag, as cargo writes it.
TEXT_FILES: List[Tuple[str, bytes, float]] = [
    (
        "Projects/rust-cli/target/CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55\n"
        b"# This file is a cache directory tag created by cargo.\n"
        b"# For information about cache directory tags see https://bford.info/cachedir/\n",
        2,
    ),
]

# Packages under web-app/node_modules: (name, files in the package's own
# folder, files in its lib/).
NODE_PACKAGES = [
    ("react", 6, 8), ("react-dom", 6, 10), ("lodash", 10, 12), ("vite", 6, 10),
    ("rollup", 5, 8), ("postcss", 5, 6), ("chalk", 4, 3), ("commander", 4, 3),
    ("debug", 4, 2), ("semver", 5, 6), ("glob", 4, 4), ("minimatch", 4, 3),
]

# Python packages in the notebook's virtual environment: (name, files).
PYTHON_PACKAGES = [("numpy", 14), ("pandas", 15), ("requests", 9), ("certifi", 3), ("six", 2)]


def entries(scale: float = 1.0) -> List[Tuple[str, int, float]]:
    """The tree's files but the text ones: (path, size, age in days), sorted."""
    out = [(p, scaled(size, scale), age) for p, size, age in FILES]
    for i, (name, own, lib) in enumerate(NODE_PACKAGES):
        base = "Projects/web-app/node_modules/%s" % name
        out.append((base + "/package.json", scaled(_pick(base, 0, 400, 1600), scale), 60))
        out.append((base + "/README.md", scaled(_pick(base, 1, 2 * K, 9 * K), scale), 60))
        for j in range(own - 2):
            out.append((base + "/index-%d.js" % j, scaled(_pick(base, 10 + j, 1 * K, 30 * K), scale), 60))
        for j in range(lib):
            out.append((base + "/lib/part-%02d.js" % j, scaled(_pick(base, 100 + j, 1 * K, 40 * K), scale), 60))
    site = "Projects/ml-notebook/.venv/lib/python3.12/site-packages"
    for name, count in PYTHON_PACKAGES:
        base = "%s/%s" % (site, name)
        for j in range(count):
            out.append((base + "/module_%02d.py" % j, scaled(_pick(base, j, 2 * K, 60 * K), scale), 90))
    for j in range(8):
        path = "Projects/rust-cli/target/debug/deps/dep_%d-%016x.d" % (j, j * 7919)
        out.append((path, scaled(_pick(path, 0, 500, 3 * K), scale), 2))
    for j in range(5):
        path = "Projects/rust-cli/target/debug/incremental/rust_cli-1a2b3c/s-%d/query-cache.bin" % j
        out.append((path, scaled(_pick(path, 0, 40 * K, 120 * K), scale), 2))
    # Documents/scans: the folder with the most files of its own (60), more
    # in two subfolders; Pictures/phone holds 50 of its own.
    for j in range(60):
        path = "Documents/scans/scan-%04d.pdf" % (j + 1)
        out.append((path, scaled(_pick(path, 0, 30 * K, 80 * K), scale), _pick(path, 1, 5, 360)))
    for year, first, ages in (("2023", 1001, (400, 1100)), ("2024", 2001, (60, 330))):
        for j in range(40):
            path = "Documents/scans/%s/scan-%04d.pdf" % (year, first + j)
            out.append((path, scaled(_pick(path, 0, 30 * K, 80 * K), scale), _pick(path, 1, *ages)))
    for j in range(8):
        path = "Documents/thesis/drafts/draft-%02d.docx" % (j + 1)
        out.append((path, scaled(_pick(path, 0, 80 * K, 200 * K), scale), 60 + 30 * j))
    for j in range(50):
        path = "Pictures/phone/IMG_%04d.heic" % (j + 1)
        out.append((path, scaled(_pick(path, 0, 50 * K, 140 * K), scale), _pick(path, 1, 1, 700)))
    for artist in ("aurora", "the-national", "khruangbin", "phoebe-bridgers"):
        for j in range(5):
            path = "Music/%s/track-%02d.flac" % (artist, j + 1)
            out.append((path, scaled(_pick(path, 0, 150 * K, 300 * K), scale), _pick(path, 1, 200, 3000)))
    return sorted(out)


# The diff task's changes between its two scans: files added, then removed.
GROWTH_ADDED: List[Tuple[str, int, float]] = [
    ("Projects/ml-notebook/checkpoints/model-v2.ckpt", 15 * M, 0.01),
    ("Downloads/zoom-installer.pkg", 4 * M, 0.01),
]
GROWTH_REMOVED: List[str] = ["Downloads/setup.dmg"]

# Directories the tree holds with nothing in them.
EMPTY_DIRS = ["Desktop", "Public"]


def scaled(size: int, scale: float) -> int:
    """`size` times `scale`, at least one byte for a file that has any."""
    if scale == 1 or size == 0:
        return size
    return max(1, int(size * scale))


def build(root: str, now: float, scale: float = 1.0) -> None:
    """Writes the tree at `root`, which must not exist yet."""
    os.makedirs(root)
    for rel in EMPTY_DIRS:
        os.makedirs(os.path.join(root, rel))
    for rel, size, age in entries(scale):
        write(root, rel, size, now - age * DAY)
    for rel, text, age in TEXT_FILES:
        path = os.path.join(root, rel)
        with open(path, "wb") as f:
            f.write(text)
        stamp = now - age * DAY
        os.utime(path, (stamp, stamp))


def grow(root: str, now: float, scale: float = 1.0) -> None:
    """Makes the changes the diff task's newer scan sees."""
    for rel, size, age in GROWTH_ADDED:
        write(root, rel, scaled(size, scale), now - age * DAY)
    for rel in GROWTH_REMOVED:
        os.remove(os.path.join(root, rel))


def write(root: str, rel: str, size: int, mtime: float) -> None:
    """One file of `size` bytes of the block, with `mtime` as both times."""
    path = os.path.join(root, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    data = _block()
    start = int.from_bytes(hashlib.sha256(rel.encode()).digest()[:4], "big") % len(data)
    with open(path, "wb") as f:
        left = size
        offset = start
        while left > 0:
            chunk = data[offset : offset + left]
            f.write(chunk)
            left -= len(chunk)
            offset = 0
    os.utime(path, (mtime, mtime))


def manifest(root: str) -> List[Tuple[str, str, int, int]]:
    """Every entry under `root`: (path, kind, size, mtime in ns), sorted. An
    agent that deleted, moved, wrote or touched anything changes it."""
    out = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for name in dirnames + filenames:
            path = os.path.join(dirpath, name)
            st = os.lstat(path)
            kind = "dir" if os.path.isdir(path) and not os.path.islink(path) else "file"
            size = 0 if kind == "dir" else st.st_size
            mtime = 0 if kind == "dir" else st.st_mtime_ns
            out.append((os.path.relpath(path, root), kind, size, mtime))
    return sorted(out)


def diff_manifests(before, after) -> List[str]:
    """What changed between two manifests, as short lines (at most ten)."""
    old = {e[0]: e for e in before}
    new = {e[0]: e for e in after}
    lines = []
    for path in sorted(set(old) | set(new)):
        if path not in new:
            lines.append("removed " + path)
        elif path not in old:
            lines.append("added " + path)
        elif old[path] != new[path]:
            lines.append("changed " + path)
    return lines[:10]


_BLOCK: Optional[bytes] = None


def _block() -> bytes:
    """One MiB of pseudorandom bytes, the same on every machine."""
    global _BLOCK
    if _BLOCK is None:
        _BLOCK = b"".join(hashlib.sha256(b"duscope-eval-%d" % i).digest() for i in range(M // 32))
    return _BLOCK


def _pick(key: str, n: int, low: float, high: float) -> int:
    """An integer in [low, high), a function of `key` and `n` alone."""
    digest = hashlib.sha256(("%s#%d" % (key, n)).encode()).digest()
    span = int(high) - int(low)
    return int(low) + int.from_bytes(digest[:8], "big") % max(1, span)


def walk_files(root: str) -> Iterator[Tuple[str, os.stat_result]]:
    """Each entry under `root` with its lstat, directories included."""
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for name in sorted(dirnames + filenames):
            path = os.path.join(dirpath, name)
            yield path, os.lstat(path)


def allocated_by_path(root: str) -> Dict[str, int]:
    """Each entry's allocated bytes, a directory's with everything below it,
    each inode counted once (by its first path), as duscope counts them."""
    own: Dict[str, int] = {}
    seen = set()
    children: Dict[str, List[str]] = {}
    st_root = os.lstat(root)
    own[root] = st_root.st_blocks * 512
    for path, st in walk_files(root):
        key = (st.st_dev, st.st_ino)
        if key in seen:
            own[path] = 0
        else:
            seen.add(key)
            own[path] = st.st_blocks * 512
        children.setdefault(os.path.dirname(path), []).append(path)
    total: Dict[str, int] = {}

    def subtree(path: str) -> int:
        size = own[path] + sum(subtree(c) for c in children.get(path, []))
        total[path] = size
        return size

    subtree(root)
    return total
