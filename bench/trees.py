"""The seeded benchmark trees T1, T2 and T3, and the generated scan T4
(bench/README.md, "The trees").

A tree is a pure function of its spec and scale: every name, size, content
byte and timestamp comes from SHA-256 over the seed, a namespace and an index,
so two builds on any platform and any Python from 3.9 on produce the same
listing. T2 follows the layout rules of fdu 0.5.0's `balanced` corpus recipe
(explorations/benchmarks/corpus.py) with fdu's own seed, so at scale 1 it is the
1M-entry tree fdu's published comparison measures.

Directories are created by the calling process; files are created by worker
processes in index ranges, each of which also returns its share of the digest.
The digest is a multiset hash (the sum of the first 16 bytes of SHA-256 of one
record line per entry, modulo 2^128), so it does not depend on creation order
and `verify` can recompute it from a walk of the tree.

T4 is never on disk: its file sizes, drawn from a log-normal spread, would
take tens of GB. It is an ncdu export, `<trees-dir>/T4.ncdu.json`, that
`duscope import` reads into the writer a scan uses (P5-BENCH-04, DEV-213),
in T2's layout with a seed of its own. Its sizes are worked out in `decimal`,
whose ln, sqrt and exp are correctly rounded on every platform, so the file is
the same bytes everywhere; its digest is the first 16 bytes of the file's
SHA-256.
"""

from __future__ import annotations

import concurrent.futures
import decimal
import hashlib
import json
import os
import shutil
import stat
import time
from dataclasses import dataclass
from typing import Dict, Iterator, List, Optional, Sequence, Tuple

# Bump when a change to this file changes any tree: stamps of other versions are
# rebuilt, and the digests pinned in duscope/tests/bench_harness.rs change.
GENERATOR_VERSION = 1
# Bump when a change to this file changes T4's export; T1-T3 are left alone.
EXPORT_VERSION = 1

# Every mtime and atime is BASE_MTIME_S plus an even number of seconds below
# 20,000, as in fdu's generator: 2023-11-14T22:13:20Z onwards.
BASE_MTIME_S = 1_700_000_000
NS = 1_000_000_000

# fdu's balanced recipe: a breadth-first directory tree of fan-out 8 holding one
# directory per eight entries, and the "standard" metadata profile.
FDU_FANOUT = 8
FDU_EXTENSIONS = (".bin", ".json", ".md", ".txt", "")
FDU_SIZES = (0, 7, 64, 257, 1_024, 4_096)

# T1's files: a home-directory mix of extensions, and fdu's "mixed" size
# profile, created sparse (apparent size only). The mean of 139,421 bytes puts
# T1 at 56 GB apparent, close to the 50 GB tree gdu's README measures, without
# allocating it.
T1_EXTENSIONS = (
    "", ".txt", ".md", ".json", ".js", ".py", ".rs", ".c",
    ".h", ".o", ".html", ".css", ".png", ".jpg", ".pdf", ".log",
)
T1_SIZES = (0, 1, 7, 64, 1_024, 4_096, 65_537, 1_048_576)

# T4's file sizes: log-normal, median 4 KiB and sigma 2.5 in natural log, so
# one file in two fits one 4 KiB block, one in a hundred is over 1.2 MiB, and
# the tree holds about 80 GB. Each file's allocated size is its size rounded up
# to whole 4 KiB blocks, as APFS and ext4 allocate. Where T2's allocated sizes
# take two values, these take thousands, so the size index's keys arrive in no
# order but the ties'.
T4_MEDIAN = 4_096
T4_SIGMA = decimal.Decimal("2.5")
T4_BLOCK = 4_096
# The root the export names: no path of the machine that made it, so every
# machine makes the same bytes. `import` reads it as given and never visits it.
T4_ROOT = "/duscope-bench/T4"
# The export's scan time: the last of the trees' mtimes, which every entry's
# precedes.
T4_SCANNED_AT_S = 1_700_020_000
_T4_CONTEXT = decimal.Context(prec=24, rounding=decimal.ROUND_HALF_EVEN)
_T4_MU = _T4_CONTEXT.ln(decimal.Decimal(T4_MEDIAN))
_HALF_53 = 1 << 52

# Below this many files a build runs in one process: spawning workers costs more.
PARALLEL_MIN_FILES = 20_000

# The file that marks a directory as one the harness builds trees in. A build
# removes `T1`, `T2`, `T3` (and scaled `Tn-xF`) there, so a `--trees-dir`
# that names an unrelated directory must stop before anything is deleted.
MARKER = ".duscope-bench-trees"

_DIGEST_MOD = 1 << 128
_CREATE = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
_UTIME_FD = os.utime in os.supports_fd


class TreeError(Exception):
    """A tree cannot be built or does not match its stamp."""


@dataclass(frozen=True)
class Spec:
    """One of the trees at scale 1."""

    name: str
    shape: str  # gdu | fdu-balanced | flat | lognormal (an export)
    seed: str
    entries: int  # entries below the root
    summary: str


# The trees built on disk, which `--trees all` names.
SPECS: Dict[str, Spec] = {
    "T1": Spec(
        "T1", "gdu", "duscope-bench-T1-v1", 500_000,
        "gdu-shaped: 100,000 directories and 400,000 sparse files",
    ),
    "T2": Spec(
        "T2", "fdu-balanced", "fdu-balanced-v1", 1_000_000,
        "fdu-shaped: fdu 0.5.0's balanced corpus at 1,000,000 entries",
    ),
    "T3": Spec(
        "T3", "flat", "duscope-bench-T3-v1", 1_000_000,
        "flat: one directory of 1,000,000 empty files",
    ),
}

# The generated scans, built as ncdu exports for `duscope import`: only the
# measurements of the write read them, and only by name.
EXPORTS: Dict[str, Spec] = {
    "T4": Spec(
        "T4", "lognormal", "duscope-bench-T4-v1", 1_000_000,
        "generated: an ncdu export of 1,000,000 entries in T2's layout, log-normal file sizes",
    ),
}


@dataclass(frozen=True)
class Tree:
    """A spec at a scale, with its entry counts."""

    spec: Spec
    scale: float
    dirs: int
    files: int

    @property
    def name(self) -> str:
        return self.spec.name

    @property
    def dirname(self) -> str:
        """The directory under the trees directory: `T1`, or `T1-x0.001` scaled."""
        if self.scale == 1:
            return self.spec.name
        return f"{self.spec.name}-x{self.scale:g}"

    @property
    def entries(self) -> int:
        return self.dirs + self.files

    @property
    def export(self) -> bool:
        """A generated scan, built as an ncdu export rather than on disk."""
        return self.spec.name in EXPORTS


def tree(name: str, scale: float = 1.0, exports: bool = False) -> Tree:
    """The tree `name` at `scale` (0 < scale <= 1): T1, T2 or T3, or with
    `exports` also T4, which only the measurements of the write read."""
    known = {**SPECS, **EXPORTS} if exports else SPECS
    spec = known.get(name)
    if spec is None:
        raise TreeError(f"unknown tree {name!r}: the trees are {', '.join(known)}")
    if not 0 < scale <= 1:
        raise TreeError(f"scale must be in (0, 1], not {scale:g}")
    entries = max(1, round(spec.entries * scale))
    if spec.shape == "gdu":
        dirs = entries // 5
    elif spec.shape in ("fdu-balanced", "lognormal"):
        # fdu's _directory_count for the balanced topology.
        dirs = 0 if entries == 1 else min(entries - 1, max(1, entries // FDU_FANOUT))
    else:
        dirs = 0
    return Tree(spec, scale, dirs, entries - dirs)


# --- The deterministic choices -------------------------------------------------


def _digest(seed: str, namespace: str, index: int) -> bytes:
    return hashlib.sha256(f"{seed}\0{namespace}\0{index}".encode("utf-8")).digest()


def stable_index(seed: str, namespace: str, index: int, length: int) -> int:
    """A choice in `0..length` (fdu's `_stable_index`)."""
    return int.from_bytes(_digest(seed, namespace, index)[:8], "big") % length


def stable_token(seed: str, namespace: str, index: int, length: int) -> str:
    """`length` hex digits (fdu's `_stable_token`)."""
    return _digest(seed, namespace, index).hex()[:length]


def mtime_ns(seed: str, relative: str) -> int:
    """The mtime (and atime) of an entry (fdu's `_mtime_ns_for`)."""
    return (BASE_MTIME_S + stable_index(seed, f"mtime:{relative}", 0, 10_000) * 2) * NS


def payload(seed: str, relative: str, size: int) -> bytes:
    """The bytes written into a T2 file (fdu's `_create_file`)."""
    if size == 0:
        return b""
    token = hashlib.sha256(f"{seed}\0payload\0{relative}".encode("utf-8")).digest()
    if size <= 4_096:
        return (token * ((size + len(token) - 1) // len(token)))[:size]
    return (token * 2)[: min(size, len(token) * 2)]  # the rest is a hole


def _join(parent: str, name: str) -> str:
    return name if parent == "." else f"{parent}/{name}"


def directory_paths(t: Tree) -> List[str]:
    """Every directory relative to the root, parents first, the root as `.`."""
    seed = t.spec.seed
    paths = ["."]
    for i in range(t.dirs):
        if t.spec.shape in ("fdu-balanced", "lognormal"):
            parent = paths[i // FDU_FANOUT]
            name = f"d-{i:07d}-{stable_token(seed, 'directory', i, 6)}"
        else:
            # A random recursive tree: each directory picks its parent uniformly
            # among those before it, which gives the skewed fan-out and the
            # depth (about ln n, at most about e ln n) of real home directories.
            parent = paths[stable_index(seed, "dir-parent", i, i + 1)]
            name = f"d{i:06d}-{stable_token(seed, 'directory', i, 4)}"
        paths.append(_join(parent, name))
    return paths


def file_entry(t: Tree, dirs: Sequence[str], index: int) -> Tuple[str, int, Optional[bytes]]:
    """File `index`: its relative path, size, and the bytes to write.

    No bytes (None) means the file is created sparse at its size.
    """
    seed = t.spec.seed
    shape = t.spec.shape
    if shape == "flat":
        return f"f{index:07d}", 0, None
    parent = dirs[stable_index(seed, "file-parent", index, len(dirs))]
    if shape == "fdu-balanced":
        extension = FDU_EXTENSIONS[stable_index(seed, "extension", index, len(FDU_EXTENSIONS))]
        name = f"f-{index:09d}-{stable_token(seed, 'file', index, 8)}{extension}"
        size = FDU_SIZES[stable_index(seed, "size", index, len(FDU_SIZES))]
        relative = _join(parent, name)
        return relative, size, payload(seed, relative, size)
    extension = T1_EXTENSIONS[stable_index(seed, "extension", index, len(T1_EXTENSIONS))]
    name = f"f{index:06d}-{stable_token(seed, 'file', index, 4)}{extension}"
    size = T1_SIZES[stable_index(seed, "size", index, len(T1_SIZES))]
    return _join(parent, name), size, None


def record_hash(kind: str, size: int, mtime: int, relative: str) -> int:
    """One entry's share of the digest. `kind` is `d` or `f`; directories have size 0."""
    line = f"{kind}\t{size}\t{mtime}\t{relative}\n".encode("utf-8", "surrogateescape")
    return int.from_bytes(hashlib.sha256(line).digest()[:16], "big")


def format_digest(total: int) -> str:
    return format(total % _DIGEST_MOD, "032x")


# --- T4: the generated scan ------------------------------------------------------


def lognormal_size(seed: str, index: int) -> int:
    """File `index`'s size in T4: exp(mu + sigma z), rounded down, with z a
    standard normal drawn by Marsaglia's polar method from SHA-256 pairs (the
    pair `attempt` of a rejected draw is the next one's namespace). Every step
    is a `decimal` operation in a context of 24 digits, correctly rounded, so
    no platform's libm enters the result."""
    ctx = _T4_CONTEXT
    attempt = 0
    while True:
        digest = _digest(seed, "size" if attempt == 0 else f"size:{attempt}", index)
        u = ctx.divide(decimal.Decimal((int.from_bytes(digest[:8], "big") >> 11) - _HALF_53), _HALF_53)
        v = ctx.divide(decimal.Decimal((int.from_bytes(digest[8:16], "big") >> 11) - _HALF_53), _HALF_53)
        s = ctx.add(ctx.multiply(u, u), ctx.multiply(v, v))
        if 0 < s < 1:
            z = ctx.multiply(u, ctx.sqrt(ctx.divide(ctx.multiply(-2, ctx.ln(s)), s)))
            x = ctx.exp(ctx.add(_T4_MU, ctx.multiply(T4_SIGMA, z)))
            return int(x.to_integral_value(rounding=decimal.ROUND_FLOOR))
        attempt += 1


def allocated_size(size: int) -> int:
    """T4's allocated size for a file of `size` bytes: whole 4 KiB blocks."""
    return -(-size // T4_BLOCK) * T4_BLOCK


def export_path(t: Tree, trees_dir: str) -> str:
    """Where T4's export lives: `<trees-dir>/T4.ncdu.json`, `T4-x0.001.ncdu.json` scaled."""
    return os.path.join(trees_dir, f"{t.dirname}.ncdu.json")


def _info(name: str, mtime_s: int, size: Optional[int] = None) -> str:
    """One ncdu info object, as compact JSON."""
    fields = f'{{"name":{json.dumps(name)}'
    if size is not None:
        fields += f',"asize":{size},"dsize":{allocated_size(size)}'
    return fields + f',"mtime":{mtime_s}}}'


_export_worker: Dict[str, object] = {}


def _init_export_worker(name: str, scale: float, dirs: List[str]) -> None:
    _export_worker["tree"] = tree(name, scale, exports=True)
    _export_worker["dirs"] = dirs


def _export_files(span: Tuple[int, int]) -> List[Tuple[int, str, int]]:
    """Files `span[0]..span[1]` of T4: each one's directory (its index in the
    directory list), its info object and its size."""
    t = _export_worker["tree"]
    dirs = _export_worker["dirs"]
    assert isinstance(t, Tree) and isinstance(dirs, list)
    seed = t.spec.seed
    out = []
    for index in range(*span):
        parent = stable_index(seed, "file-parent", index, len(dirs))
        extension = FDU_EXTENSIONS[stable_index(seed, "extension", index, len(FDU_EXTENSIONS))]
        name = f"f-{index:09d}-{stable_token(seed, 'file', index, 8)}{extension}"
        size = lognormal_size(seed, index)
        mtime_s = mtime_ns(seed, _join(dirs[parent], name)) // NS
        out.append((parent, _info(name, mtime_s, size), size))
    return out


def write_export(t: Tree, path: str, jobs: int) -> int:
    """Writes T4 as an ncdu JSON export (dev.yorhel.nl/ncdu/jsonfmt, major
    version 1, minor 2) to `path`; returns the files' apparent bytes. Each
    directory lists its files in index order, then its subdirectories. The
    root is T4_ROOT and the scan time T4_SCANNED_AT_S, so the bytes depend on
    the spec and the scale alone."""
    seed = t.spec.seed
    dirs = directory_paths(t)
    jobs = max(1, jobs)
    if jobs == 1 or t.files < PARALLEL_MIN_FILES:
        _init_export_worker(t.name, t.scale, dirs)
        parts = [_export_files((0, t.files))] if t.files else []
    else:
        with concurrent.futures.ProcessPoolExecutor(
            max_workers=jobs, initializer=_init_export_worker, initargs=(t.name, t.scale, dirs),
        ) as pool:
            parts = list(pool.map(_export_files, _spans(t.files, jobs * 8)))
    files: List[List[str]] = [[] for _ in dirs]
    apparent = 0
    for part in parts:
        for parent, info, size in part:
            files[parent].append(info)
            apparent += size
    # Directory i below the root is dirs[i + 1], and its parent is dirs[i // 8].
    children: List[List[int]] = [[] for _ in dirs]
    for i in range(t.dirs):
        children[i // FDU_FANOUT].append(i + 1)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        meta = {"progname": "duscope-bench", "progver": f"{t.dirname}-v{EXPORT_VERSION}", "timestamp": T4_SCANNED_AT_S}
        f.write(f"[1,2,{json.dumps(meta, separators=(',', ':'))},\n")
        # Depth first, without recursion: each directory opens its array, lists
        # its files, and its subdirectories follow before the array closes.
        stack: List[Tuple[int, bool]] = [(0, False)]
        first = True
        while stack:
            at, done = stack.pop()
            if done:
                f.write("]")
                continue
            name = T4_ROOT if at == 0 else dirs[at].rsplit("/", 1)[-1]
            f.write(("" if first else ",\n") + "[" + _info(name, mtime_ns(seed, dirs[at]) // NS))
            first = False
            for info in files[at]:
                f.write(",\n" + info)
            stack.append((at, True))
            stack.extend((child, False) for child in reversed(children[at]))
        f.write("]\n")
    return apparent


def file_digest(path: str) -> str:
    """T4's digest: the first 16 bytes of its file's SHA-256, as hex."""
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()[:32]


# --- Building ------------------------------------------------------------------

_worker: Dict[str, object] = {}


def _init_worker(root: str, name: str, scale: float, dirs: List[str]) -> None:
    _worker["root"] = root
    _worker["tree"] = tree(name, scale)
    _worker["dirs"] = dirs


def _make_files(span: Tuple[int, int]) -> Tuple[int, int]:
    """Create files `span[0]..span[1]`; return their apparent bytes and digest share."""
    root = str(_worker["root"])
    t = _worker["tree"]
    dirs = _worker["dirs"]
    assert isinstance(t, Tree) and isinstance(dirs, list)
    seed = t.spec.seed
    apparent = 0
    digest = 0
    for index in range(*span):
        relative, size, data = file_entry(t, dirs, index)
        mtime = mtime_ns(seed, relative)
        path = f"{root}/{relative}"
        fd = os.open(path, _CREATE, 0o644)
        try:
            if data:
                view = memoryview(data)
                while view:
                    view = view[os.write(fd, view):]
            if size > len(data or b""):
                os.ftruncate(fd, size)
            if _UTIME_FD:
                os.utime(fd, ns=(mtime, mtime))
        finally:
            os.close(fd)
        if not _UTIME_FD:
            os.utime(path, ns=(mtime, mtime))
        apparent += size
        digest += record_hash("f", size, mtime, relative)
    return apparent, digest % _DIGEST_MOD


def _spans(count: int, pieces: int) -> List[Tuple[int, int]]:
    pieces = max(1, min(pieces, count))
    step = -(-count // pieces)
    return [(start, min(start + step, count)) for start in range(0, count, step)]


def _check_sparse(directory: str) -> None:
    """Refuse to build T1 where a sparse file would allocate its apparent size."""
    probe = os.path.join(directory, ".sparse-probe")
    fd = os.open(probe, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    try:
        os.ftruncate(fd, 1 << 20)
        allocated = os.fstat(fd).st_blocks * 512
    finally:
        os.close(fd)
        os.unlink(probe)
    if allocated >= 1 << 19:
        raise TreeError(
            f"the filesystem under {directory} does not keep sparse files sparse "
            f"(a 1 MiB hole allocated {allocated} bytes), and T1's 56 GB of sparse "
            "files would be written out; put --trees-dir on APFS, ext4, XFS or btrfs"
        )


def stamp_path(t: Tree, trees_dir: str) -> str:
    return os.path.join(trees_dir, f"{t.dirname}.json")


def tree_root(t: Tree, trees_dir: str) -> str:
    return os.path.join(trees_dir, t.dirname)


def _identity(t: Tree) -> Dict[str, object]:
    identity: Dict[str, object] = {
        "generator_version": GENERATOR_VERSION,
        "name": t.name,
        "shape": t.spec.shape,
        "seed": t.spec.seed,
        "scale": t.scale,
        "dirs": t.dirs,
        "files": t.files,
    }
    if t.export:
        identity["export_version"] = EXPORT_VERSION
    return identity


def built_path(t: Tree, trees_dir: str) -> str:
    """What a build of `t` makes: the tree's root directory, or T4's export."""
    return export_path(t, trees_dir) if t.export else tree_root(t, trees_dir)


def read_stamp(t: Tree, trees_dir: str) -> Optional[Dict[str, object]]:
    """The stamp of a complete build of exactly this tree, or None."""
    try:
        with open(stamp_path(t, trees_dir), encoding="utf-8") as f:
            stamp = json.load(f)
    except (OSError, ValueError):
        return None
    there = os.path.isfile if t.export else os.path.isdir
    if not isinstance(stamp, dict) or not there(built_path(t, trees_dir)):
        return None
    if any(stamp.get(k) != v for k, v in _identity(t).items()):
        return None
    return stamp


def _remove(path: str) -> None:
    if os.path.lexists(path):
        if os.path.isdir(path) and not os.path.islink(path):
            shutil.rmtree(path)
        else:
            os.unlink(path)


def _has_stamp(trees_dir: str) -> bool:
    """Whether `trees_dir` holds a build stamp (a directory used before the marker existed)."""
    for name in os.listdir(trees_dir):
        if name.endswith(".json"):
            try:
                with open(os.path.join(trees_dir, name), encoding="utf-8") as f:
                    if "generator_version" in json.load(f):
                        return True
            except (OSError, ValueError, TypeError):
                continue
    return False


def claim(trees_dir: str) -> None:
    """Create `trees_dir` if needed and make sure builds may replace trees in it."""
    marker = os.path.join(trees_dir, MARKER)
    if os.path.isfile(marker):
        return
    os.makedirs(trees_dir, exist_ok=True)
    if os.listdir(trees_dir) and not _has_stamp(trees_dir):
        raise TreeError(
            f"{trees_dir} is not a directory of benchmark trees (it holds no {MARKER} and no "
            "build stamp, and is not empty): pass a new or empty --trees-dir"
        )
    with open(marker, "w", encoding="utf-8") as f:
        f.write("duscope benchmark trees\n")


def build(t: Tree, trees_dir: str, jobs: int, force: bool = False, log=None) -> Dict[str, object]:
    """Build `t` under `trees_dir` unless a matching stamp says it is there, and a walk of the
    tree still gives the stamp's digest; return the stamp."""
    say = log or (lambda message: None)
    root = tree_root(t, trees_dir)
    if not force:
        stamp = read_stamp(t, trees_dir)
        if stamp is not None:
            try:
                observed = current_digest(t, trees_dir)
            except (OSError, TreeError) as e:
                observed = f"unreadable ({e})"
            if observed == stamp["digest"]:
                say(f"{t.dirname}: up to date ({t.dirs:,} directories, {t.files:,} files)")
                return stamp
            say(f"{t.dirname}: changed since it was built (walk {observed}, stamp {stamp['digest']}); rebuilding")
    claim(trees_dir)
    if t.export:
        return _build_export(t, trees_dir, jobs, say)
    # Before anything is removed: a filesystem that cannot hold the tree keeps
    # the previous build.
    if t.spec.shape == "gdu":
        _check_sparse(trees_dir)
    # The stamp goes first, so an interrupted rebuild never leaves a stale one.
    _remove(stamp_path(t, trees_dir))
    if os.path.lexists(root):
        say(f"{t.dirname}: removing the previous build")
        _remove(root)
    say(f"{t.dirname}: building {t.dirs:,} directories and {t.files:,} files")
    started = time.monotonic()
    seed = t.spec.seed
    os.mkdir(root, 0o755)
    dirs = directory_paths(t)
    digest = record_hash("d", 0, mtime_ns(seed, "."), ".")
    for relative in dirs[1:]:
        os.mkdir(f"{root}/{relative}", 0o755)
        digest += record_hash("d", 0, mtime_ns(seed, relative), relative)

    jobs = max(1, jobs)
    if jobs == 1 or t.files < PARALLEL_MIN_FILES:
        _init_worker(root, t.name, t.scale, dirs)
        parts = [_make_files((0, t.files))] if t.files else []
    else:
        with concurrent.futures.ProcessPoolExecutor(
            max_workers=jobs,
            initializer=_init_worker,
            initargs=(root, t.name, t.scale, dirs),
        ) as pool:
            parts = list(pool.map(_make_files, _spans(t.files, jobs * 8)))
    apparent = sum(part[0] for part in parts)
    digest += sum(part[1] for part in parts)

    # Creating entries moved the directories' times; set them last. Setting one
    # directory's times changes no other directory's, so the order is free.
    for relative in dirs:
        mtime = mtime_ns(seed, relative)
        os.utime(root if relative == "." else f"{root}/{relative}", ns=(mtime, mtime))
    elapsed = time.monotonic() - started

    return _write_stamp(t, trees_dir, apparent, format_digest(digest), elapsed, jobs, say)


def _write_stamp(t: Tree, trees_dir: str, apparent: int, digest: str, elapsed: float, jobs: int, say) -> Dict[str, object]:
    stamp: Dict[str, object] = dict(_identity(t))
    stamp.update(
        {
            "apparent_bytes": apparent,
            "digest": digest,
            "build_s": round(elapsed, 3),
            "built_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "jobs": jobs,
        }
    )
    temporary = stamp_path(t, trees_dir) + ".tmp"
    with open(temporary, "w", encoding="utf-8") as f:
        json.dump(stamp, f, indent=2)
        f.write("\n")
    os.replace(temporary, stamp_path(t, trees_dir))
    say(f"{t.dirname}: built in {elapsed:.1f} s, digest {stamp['digest']}")
    return stamp


def _build_export(t: Tree, trees_dir: str, jobs: int, say) -> Dict[str, object]:
    """Writes T4's export and its stamp; the stamp goes first and the file is
    renamed into place whole, so an interrupted build leaves neither stale."""
    path = export_path(t, trees_dir)
    _remove(stamp_path(t, trees_dir))
    say(f"{t.dirname}: generating an ncdu export of {t.dirs:,} directories and {t.files:,} files")
    started = time.monotonic()
    temporary = path + ".tmp"
    try:
        apparent = write_export(t, temporary, jobs)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    elapsed = time.monotonic() - started
    return _write_stamp(t, trees_dir, apparent, file_digest(path), elapsed, max(1, jobs), say)


# --- Verifying -----------------------------------------------------------------


def walk(root: str) -> Iterator[Tuple[str, int, int, str]]:
    """Every entry under `root` as (kind, size, mtime_ns, relative path), root included."""
    yield "d", 0, os.lstat(root).st_mtime_ns, "."
    stack = [(root, ".")]
    while stack:
        directory, relative = stack.pop()
        with os.scandir(directory) as entries:
            for entry in entries:
                child = _join(relative, entry.name)
                st = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(st.st_mode):
                    yield "d", 0, st.st_mtime_ns, child
                    stack.append((entry.path, child))
                elif stat.S_ISREG(st.st_mode):
                    yield "f", st.st_size, st.st_mtime_ns, child
                else:
                    raise TreeError(f"{entry.path}: not a regular file or directory")


def walk_digest(root: str) -> str:
    """The digest of the tree under `root` as it is now."""
    total = 0
    for kind, size, mtime, relative in walk(root):
        total += record_hash(kind, size, mtime, relative)
    return format_digest(total)


def current_digest(t: Tree, trees_dir: str) -> str:
    """The digest of what a build of `t` left in `trees_dir`, as it is now: a
    walk of the tree, or T4's file read whole."""
    if t.export:
        return file_digest(export_path(t, trees_dir))
    return walk_digest(tree_root(t, trees_dir))


def verify(t: Tree, trees_dir: str) -> Tuple[str, str]:
    """Walk the built tree; return (digest of the walk, digest in the stamp)."""
    stamp = read_stamp(t, trees_dir)
    if stamp is None:
        raise TreeError(f"{t.dirname} is not built under {trees_dir} (run `bench/run.sh build`)")
    return current_digest(t, trees_dir), str(stamp["digest"])
