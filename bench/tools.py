"""The pinned tools the harness measures, and their harness-local installation.

Every pinned tool installs under the tools directory (bench/.tools by default),
never globally: release archives are downloaded with curl, checked against the
SHA-256 recorded here and unpacked into <tools-dir>/bin; a tool without a
release binary for this platform is built with `cargo install --locked --root
<tools-dir>` from crates.io (whose checksums cargo verifies) or from a git
commit. A tool that cannot run on this platform is skipped with its reason.
bench/README.md, "The tools", mirrors the PINS table; a test compares them.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import shlex
import shutil
import subprocess
import tarfile
import tempfile
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

USER_AGENT = "duscope-bench"

# The platform keys of release assets.
PLATFORMS = ("macos-arm64", "macos-x86_64", "linux-x86_64", "linux-aarch64")


class ToolError(Exception):
    """A tool cannot be installed or does not answer as pinned."""


@dataclass(frozen=True)
class Release:
    """A release asset: a tar.gz holding `member`, or the bare binary when `member` is None."""

    url: str
    sha256: str
    member: Optional[str] = None


@dataclass(frozen=True)
class Cargo:
    """A `cargo install` source: a crates.io version, or a git commit."""

    crate: str
    version: Optional[str] = None
    git: Optional[str] = None
    rev: Optional[str] = None


@dataclass(frozen=True)
class Tool:
    name: str
    pinned: Optional[str]  # None for the system du
    args: Tuple[str, ...]  # the command; "{tree}" is replaced by the tree's path
    releases: Dict[str, Release] = field(default_factory=dict)
    cargo: Optional[Cargo] = None
    system: Optional[str] = None  # an absolute path on every platform it supports
    only_on: Optional[str] = None  # "macos" or "linux"
    skip: Optional[str] = None  # why this tool is never installed by the script
    version_args: Optional[Tuple[str, ...]] = ("--version",)


def _gh(repo: str, tag: str, asset: str) -> str:
    return f"https://github.com/{repo}/releases/download/{tag}/{asset}"


def _tarballs(repo: str, tag: str, stem: str, binary: str, targets: Dict[str, Tuple[str, str]]) -> Dict[str, Release]:
    """Releases named `<stem>-<target>.tar.gz` holding `<stem>-<target>/<binary>`."""
    return {
        key: Release(_gh(repo, tag, f"{stem}-{target}.tar.gz"), sha, f"{stem}-{target}/{binary}")
        for key, (target, sha) in targets.items()
    }


# The competitors are pinned as plan/PROPOSAL.md's 1.0 acceptance targets list
# them. The release digests are the SHA-256 GitHub records for each asset.
PINS: Dict[str, Tool] = {
    "du": Tool(
        "du", None, ("du", "-sk", "{tree}"), system="/usr/bin/du",
        version_args=("--version",),
    ),
    "diskus": Tool(
        "diskus", "0.9.0", ("diskus", "{tree}"),
        releases=_tarballs("sharkdp/diskus", "v0.9.0", "diskus-v0.9.0", "diskus", {
            "macos-arm64": ("aarch64-apple-darwin", "85d5b4f79c0986e1c25ce35b4df5376c9d4a40ce178b94f7a4962ed629999a7a"),
            "macos-x86_64": ("x86_64-apple-darwin", "6f83be20575ad8227758681f0b0021d5fbbf1019867ba418a43177e18a0152cb"),
            "linux-x86_64": ("x86_64-unknown-linux-musl", "6ed6fbbd8b14c9daf2e56e690ee16761ceea5bf73b48813b3d723d8498146dc4"),
            "linux-aarch64": ("aarch64-unknown-linux-gnu", "de8d7b4147911ed228fffeb49e4d19bc42c23a65eb4e3db98058ad9778a835cb"),
        }),
        cargo=Cargo("diskus", "0.9.0"),
    ),
    "dua": Tool(
        "dua", "2.46.0", ("dua", "{tree}"),
        releases=_tarballs("Byron/dua-cli", "v2.46.0", "dua-v2.46.0", "dua", {
            "macos-arm64": ("aarch64-apple-darwin", "857af70a6be7fe3d22611a3aa082f17789435ad0b89808845ef13f85789e8e1b"),
            "macos-x86_64": ("x86_64-apple-darwin", "1fe7c6bd3cc1f3d37ee78d22a9f5ec5bd6d33eb24e5c8bf5864aa7efa03f8324"),
            "linux-x86_64": ("x86_64-unknown-linux-musl", "4f284a388a063a99f938c6e0f5ea8d932868e1f9a4224622d97488f4ec671b41"),
            "linux-aarch64": ("aarch64-unknown-linux-musl", "a7ba6bce1f1d16392b4c9f385e7439deda71627ed3c55eb09d5dfcbe6b481a58"),
        }),
        cargo=Cargo("dua-cli", "2.46.0"),
    ),
    "pdu": Tool(
        "pdu", "0.24.0", ("pdu", "{tree}"),
        # pdu publishes x86_64 binaries only; arm64 builds from crates.io.
        releases={
            "macos-x86_64": Release(
                _gh("KSXGitHub/parallel-disk-usage", "0.24.0", "pdu-x86_64-apple-darwin"),
                "43f0e13932e4a28231e322f228f0f7bd8dae53773f24ba5ffa2f548bf4627972"),
            "linux-x86_64": Release(
                _gh("KSXGitHub/parallel-disk-usage", "0.24.0", "pdu-x86_64-unknown-linux-musl"),
                "b991a68c1555d9144ea8ef2dd8cab91efb0b90ec8be53fdec4d0b2dd0e596f56"),
        },
        cargo=Cargo("parallel-disk-usage", "0.24.0"),
    ),
    "dust": Tool(
        "dust", "1.2.6", ("dust", "-d0", "{tree}"),
        releases=_tarballs("bootandy/dust", "v1.2.6", "dust-v1.2.6", "dust", {
            "macos-arm64": ("aarch64-apple-darwin", "5c37ee0d353165c73aedd6b21b9133182221efe08f9a5e76c082d9d6d5a0935d"),
            "macos-x86_64": ("x86_64-apple-darwin", "7603c92462bd56305847732eb25d70ab06386d7bcfbce7532b7cf3937fd9684f"),
            "linux-x86_64": ("x86_64-unknown-linux-musl", "cbf2cb5c9d6227e46fc4b29c03dc43b8632020dfc5cf269d263d6e29c399e044"),
            "linux-aarch64": ("aarch64-unknown-linux-musl", "a51cf42a4654864837cc116eaca914271d7a09a9c2a60de90ecc2187b8d855a7"),
        }),
        cargo=Cargo("du-dust", "1.2.6"),
    ),
    "gdu": Tool(
        "gdu", "5.38.0", ("gdu", "-npc", "{tree}"),
        releases={
            key: Release(_gh("dundee/gdu", "v5.38.0", f"{asset}.tgz"), sha, asset)
            for key, (asset, sha) in {
                "macos-arm64": ("gdu_darwin_arm64", "cbfc4981f6d5d5bf66a1dbb1faa65bc64b15060b2782b325b888f616655415db"),
                "macos-x86_64": ("gdu_darwin_amd64", "713469aa9f3bbeef0cd26d0bc7982c9cba6027a0f0b83ea19cec6f026f8c624a"),
                "linux-x86_64": ("gdu_linux_amd64_static", "e7b33d9a04597ac2caa2d7ff244023735617bb029a046ca5cf6154ef6bc1d4fe"),
                "linux-aarch64": ("gdu_linux_arm64", "72364ae2e0b990cd5e9651f2d11995c40969b69512fb2f5d79b56839f66fb904"),
            }.items()
        },
    ),
    "ncdu": Tool(
        "ncdu", "2.9.2", ("ncdu", "-0", "-o", "/dev/null", "{tree}"),
        skip=(
            "ncdu 2.9.2 publishes no prebuilt binary (the static Linux builds stop at "
            "2.9.1) and building it needs Zig 0.15 with the ncurses and zstd headers; "
            "pass --tool-path ncdu=PATH to measure a 2.9.2 build"
        ),
    ),
    "fdu": Tool(
        "fdu", "0.5.0", ("fdu", "{tree}"),
        cargo=Cargo("fdu", "0.5.0"),
    ),
    "dumac": Tool(
        "dumac", "1ffbe3c38d1066c45cac9b4ec1e31eb74edc1076", ("dumac", "{tree}"),
        cargo=Cargo("dumac", git="https://github.com/healeycodes/dumac", rev="1ffbe3c38d1066c45cac9b4ec1e31eb74edc1076"),
        only_on="macos",
        version_args=None,  # dumac takes no flags; the commit is the version
    ),
}

COMPETITORS = tuple(name for name in PINS if name != "du")


def platform_key() -> str:
    """`macos-arm64`, `linux-x86_64`, ...; raises ToolError elsewhere."""
    system = platform.system()
    machine = platform.machine().lower()
    os_name = {"Darwin": "macos", "Linux": "linux"}.get(system)
    arch = {"arm64": "arm64", "aarch64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}.get(machine)
    if os_name is None or arch is None:
        raise ToolError(f"unsupported platform {system} {machine}: the harness runs on macOS and Linux")
    if os_name == "macos" and arch == "aarch64":
        arch = "arm64"
    if os_name == "linux" and arch == "arm64":
        arch = "aarch64"
    return f"{os_name}-{arch}"


def source_for(tool: Tool, key: str) -> Tuple[str, Optional[str]]:
    """How `tool` installs on platform `key`: (source, reason it does not)."""
    if tool.only_on and not key.startswith(tool.only_on + "-"):
        return "skipped", f"{tool.name} runs on {'macOS' if tool.only_on == 'macos' else 'Linux'} only"
    if tool.skip:
        return "skipped", tool.skip
    if tool.system:
        return "system", None
    if key in tool.releases:
        return "release", None
    if tool.cargo and tool.cargo.git:
        return "git", None
    if tool.cargo:
        return "cargo", None
    return "skipped", f"{tool.name} {tool.pinned} has no build for {key}"


@dataclass
class Resolved:
    """A tool as this run found it."""

    tool: Tool
    status: str  # ready | skipped | failed
    source: str  # system | release | cargo | git | path | command | skipped
    path: Optional[str] = None
    version: Optional[str] = None
    reason: Optional[str] = None
    argv_template: Tuple[str, ...] = ()

    def command(self) -> str:
        """The command line as the result file records it: the tool's name, `{tree}` for the tree."""
        template = self.argv_template or self.tool.args
        return " ".join(p if p == "{tree}" else shlex.quote(p) for p in template)

    def argv(self, tree_path: str) -> List[str]:
        template = self.argv_template or self.tool.args
        argv = [part.replace("{tree}", tree_path) for part in template]
        if self.path:
            argv[0] = self.path
        return argv


def first_line(argv: List[str]) -> Optional[str]:
    """The first non-empty output line of a version command, or None when it fails."""
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=30, errors="replace")
    except (OSError, subprocess.TimeoutExpired):
        return None
    if done.returncode != 0:
        return None
    for line in (done.stdout + "\n" + done.stderr).splitlines():
        if line.strip():
            return " ".join(line.split())
    return None


def _marker_path(tools_dir: str, name: str) -> str:
    return os.path.join(tools_dir, "installed", f"{name}.json")


def _read_marker(tools_dir: str, name: str) -> Optional[Dict[str, str]]:
    try:
        with open(_marker_path(tools_dir, name), encoding="utf-8") as f:
            marker = json.load(f)
    except (OSError, ValueError):
        return None
    return marker if isinstance(marker, dict) else None


def _write_marker(tools_dir: str, name: str, marker: Dict[str, str]) -> None:
    path = _marker_path(tools_dir, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(marker, f, indent=2, sort_keys=True)
        f.write("\n")


def check_version(tool: Tool, path: str) -> Optional[str]:
    """The version line of `path`, which must name the pinned version.

    A tool without a pin (du) may have no version line: BSD du takes no --version.
    """
    if tool.version_args is None:
        return tool.pinned
    line = first_line([path, *tool.version_args])
    if line is None:
        if tool.pinned is None:
            return None
        raise ToolError(f"`{path} {' '.join(tool.version_args)}` failed")
    if tool.pinned and tool.pinned not in line:
        raise ToolError(f"{path} reports `{line}`, not the pinned {tool.pinned}")
    return line


def _sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _download(url: str, sha256: str, downloads: str, log) -> str:
    os.makedirs(downloads, exist_ok=True)
    target = os.path.join(downloads, url.rsplit("/", 1)[-1])
    if os.path.isfile(target) and _sha256(target) == sha256:
        return target
    if shutil.which("curl") is None:
        raise ToolError("curl is needed to download release binaries")
    log(f"downloading {url}")
    partial = target + ".part"
    done = subprocess.run(
        ["curl", "-fsSL", "--retry", "3", "-A", USER_AGENT, "-o", partial, url],
        capture_output=True, text=True, errors="replace",
    )
    if done.returncode != 0:
        raise ToolError(f"downloading {url} failed: {done.stderr.strip()}")
    actual = _sha256(partial)
    if actual != sha256:
        os.unlink(partial)
        raise ToolError(f"{url} has SHA-256 {actual}, not the pinned {sha256}")
    os.replace(partial, target)
    return target


def _install_release(tool: Tool, release: Release, tools_dir: str, log) -> str:
    archive = _download(release.url, release.sha256, os.path.join(tools_dir, "downloads"), log)
    binary = os.path.join(tools_dir, "bin", tool.name)
    os.makedirs(os.path.dirname(binary), exist_ok=True)
    partial = binary + ".part"
    if release.member is None:
        shutil.copyfile(archive, partial)
    else:
        # Only the one member is read, by name, so the archive's other paths
        # never reach the filesystem.
        with tarfile.open(archive, "r:gz") as tar:
            try:
                member = tar.getmember(release.member)
            except KeyError:
                raise ToolError(f"{release.url} has no {release.member}") from None
            source = tar.extractfile(member)
            if source is None:
                raise ToolError(f"{release.member} in {release.url} is not a file")
            with source, open(partial, "wb") as out:
                shutil.copyfileobj(source, out)
    os.chmod(partial, 0o755)
    os.replace(partial, binary)
    return binary


def _install_cargo(tool: Tool, cargo: Cargo, tools_dir: str, log) -> str:
    command = ["cargo", "install", "--locked", "--quiet", "--root", os.path.abspath(tools_dir)]
    if cargo.git:
        command += ["--git", str(cargo.git), "--rev", str(cargo.rev), cargo.crate]
    else:
        command += [f"{cargo.crate}@{cargo.version}"]
    # The stable toolchain, as `just setup` uses for its tools: the repository's
    # rust-toolchain.toml pins 1.88, older than some competitors need.
    if shutil.which("rustup"):
        command = ["rustup", "run", "stable", *command]
    elif shutil.which("cargo") is None:
        raise ToolError("cargo is needed to build it: install rustup (https://rustup.rs)")
    log(f"building {tool.name} {tool.pinned}: {' '.join(command)}")
    with tempfile.TemporaryDirectory(prefix="duscope-bench-cargo-") as scratch:
        environment = dict(os.environ, CARGO_TARGET_DIR=os.path.join(scratch, "target"))
        done = subprocess.run(command, cwd=scratch, env=environment, capture_output=True, text=True, errors="replace")
    if done.returncode != 0:
        tail = "\n".join(done.stderr.strip().splitlines()[-5:])
        raise ToolError(f"cargo install failed: {tail}")
    binary = os.path.join(tools_dir, "bin", tool.name)
    if not os.path.isfile(binary):
        raise ToolError(f"cargo install did not produce {binary}")
    return binary


def resolve(tool: Tool, key: str, tools_dir: str, install: bool, override: Optional[str], log) -> Resolved:
    """Find, or install, `tool` for platform `key`."""
    if override:
        path = shutil.which(override) or override
        try:
            return Resolved(tool, "ready", "path", path, check_version(tool, path))
        except ToolError as e:
            return Resolved(tool, "failed", "path", path, reason=str(e))
    source, reason = source_for(tool, key)
    if source == "skipped":
        return Resolved(tool, "skipped", source, reason=reason)
    if source == "system":
        path = str(tool.system)
        if not os.access(path, os.X_OK):
            return Resolved(tool, "failed", source, reason=f"{path} is not executable")
        # BSD du has no --version; GNU du names its coreutils release.
        return Resolved(tool, "ready", source, path, check_version(tool, path))

    binary = os.path.join(tools_dir, "bin", tool.name)
    marker = _read_marker(tools_dir, tool.name)
    if marker and marker.get("pinned") == tool.pinned and os.access(binary, os.X_OK):
        try:
            return Resolved(tool, "ready", source, binary, check_version(tool, binary))
        except ToolError:
            pass  # reinstall below
    if not install:
        return Resolved(tool, "failed", source, reason=f"not installed under {tools_dir}")
    try:
        if source == "release":
            path = _install_release(tool, tool.releases[key], tools_dir, log)
        else:
            assert tool.cargo is not None
            path = _install_cargo(tool, tool.cargo, tools_dir, log)
        version = check_version(tool, path)
    except (ToolError, OSError, tarfile.TarError) as e:
        return Resolved(tool, "failed", source, reason=str(e))
    _write_marker(tools_dir, tool.name, {"pinned": str(tool.pinned), "source": source})
    log(f"{tool.name}: installed {version}")
    return Resolved(tool, "ready", source, path, version)


def ad_hoc(name: str, command: str) -> Resolved:
    """A `--command NAME=CMD` tool: a command line measured beside the pinned ones."""
    argv = tuple(shlex.split(command))
    if not argv:
        raise ToolError(f"--command {name}= needs a command line")
    tool = Tool(name, None, argv, version_args=None)
    path = shutil.which(argv[0])
    if path is None:
        return Resolved(tool, "failed", "command", reason=f"{argv[0]} is not on PATH")
    return Resolved(tool, "ready", "command", path, None, argv_template=argv)
