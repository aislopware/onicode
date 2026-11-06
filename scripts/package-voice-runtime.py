#!/usr/bin/env python3
"""Package the pinned native voice runtime without installing or copying the CLI."""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
from pathlib import Path, PurePosixPath

BUILD = "a956835d020762cb2b570053af06f643a11c0ecc"
VERSION = "0.160.0"
PACKAGES = {
    "aarch64-apple-darwin": ("aarch64-apple-darwin", None),
    "x86_64-unknown-linux-gnu": (
        "x86_64-unknown-linux-musl",
        "4fcc47ab57f52ff75363951a8761146cd10c8288bd86fed45487dbb204a16b71",
    ),
    "aarch64-unknown-linux-gnu": (
        "aarch64-unknown-linux-musl",
        "7f0fe42ff22ecfa3a47bc4a34f5b22c4218b431a4ec0aba51c7d98299f07900c",
    ),
}


def host_target() -> str:
    """Return the voice target of this machine."""
    arch = {"amd64": "x86_64", "arm64": "aarch64"}.get(platform.machine().lower(), platform.machine().lower())
    suffix = {"Darwin": "apple-darwin", "Linux": "unknown-linux-gnu"}.get(platform.system())
    target = f"{arch}-{suffix}"
    if target not in PACKAGES:
        msg = f"unsupported voice target: {target}"
        raise ValueError(msg)
    return target


def relative_path(name: str) -> Path:
    """Parse a manifest path, refusing anything that could leave the package."""
    path = PurePosixPath(name)
    if (
        not name
        or "\\" in name
        or ":" in name
        or path.is_absolute()
        or ".." in path.parts
        or path.as_posix() != name
        or name == "."
    ):
        msg = "invalid manifest path"
        raise ValueError(msg)
    return Path(*path.parts)


def checksum(path: Path) -> str:
    """Return the file's SHA-256, hex."""
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def download_package(directory: Path, target: str) -> Path:
    """Download and unpack the pinned Codex package for `target`; the package directory."""
    app_target, digest = PACKAGES[target]
    if digest is None:
        msg = "supply an explicit source package for macOS"
        raise ValueError(msg)
    name = f"codex-package-{app_target}.tar.gz"
    archive = directory / name
    url = f"https://github.com/openai/codex/releases/download/rust-v{VERSION}/{name}"
    # Only the build machine downloads this fixed, checksum-pinned release.
    with urllib.request.urlopen(url, timeout=120) as response, archive.open("wb") as output:
        shutil.copyfileobj(response, output)
    if checksum(archive) != digest:
        msg = "voice archive checksum mismatch"
        raise ValueError(msg)
    package = directory / "package"
    package.mkdir()
    with tarfile.open(archive) as source:
        source.extractall(package, filter="data")
    return package


def verify(package: Path, target: str) -> Path:
    """Check the package is the pinned build for `target` and holds only what its manifest lists.

    Returns the voice runtime directory inside it.
    """
    app_target, _ = PACKAGES[target]
    metadata = json.loads((package / "codex-package.json").read_text())
    if (metadata["layoutVersion"], metadata["version"], metadata["target"]) != (1, VERSION, app_target):
        msg = "unsupported Codex package layout, version or target"
        raise ValueError(msg)
    source = package / "codex-resources/voice"
    manifest = json.loads((source / "manifest.json").read_text())
    if (manifest["buildCommit"], manifest["appVersion"], manifest["appTarget"], manifest["voiceTarget"]) != (
        BUILD,
        VERSION,
        app_target,
        target,
    ):
        msg = f"unsupported voice package; expected pinned {VERSION} {target}"
        raise ValueError(msg)
    for relative, digest in manifest["sha256"].items():
        path = package / relative_path(relative)
        if checksum(path) != digest:
            msg = "voice package checksum mismatch"
            raise ValueError(msg)
    # Reject untracked runtime files and symlinks before signing/copying code.
    for path in source.rglob("*"):
        if path.is_symlink():
            msg = "voice runtime symlinks are unsupported"
            raise ValueError(msg)
        if (
            path.is_file()
            and path.name != "manifest.json"
            and path.relative_to(package).as_posix() not in manifest["sha256"]
        ):
            msg = "untracked runtime resource"
            raise ValueError(msg)
    for required in ["bin/codex-voice-host", "runtime.json", "NOTICE.md", "sources.json"]:
        if not (source / required).is_file():
            msg = f"missing runtime resource: {required}"
            raise ValueError(msg)
    return source


def install(package: Path, destination: Path, target: str | None = None) -> None:
    """Verify `package` and install its voice runtime at `destination`, replacing what is there."""
    target = target or host_target()
    # The helper only initializes from a `codex-resources/voice` directory; it
    # exits with code 23 on initializeRuntime anywhere else.
    if (destination.parent.name, destination.name) != ("codex-resources", "voice"):
        msg = "destination must end in codex-resources/voice"
        raise ValueError(msg)
    source = verify(package, target)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        stage = Path(temporary) / "voice"
        shutil.copytree(source, stage)
        shutil.copyfile(
            Path(__file__).resolve().parents[1] / "packaging/voice/Codex-LICENSE.txt",
            stage / "licenses/Codex-LICENSE.txt",
        )
        # Ship the runtime byte-for-byte: upstream already signs the helper and
        # every library with OpenAI's Developer ID, hardened runtime and a
        # secure timestamp, so re-signing would only downgrade them.
        runtime = json.loads((stage / "runtime.json").read_text())
        if (runtime["target"], runtime["sourceCommit"]) != (target, BUILD):
            msg = "incompatible native runtime"
            raise ValueError(msg)
        for library in runtime["libraries"]:
            if checksum(stage / relative_path(library["path"])) != library["sha256"]:
                msg = f"runtime library does not match runtime.json: {library['path']}"
                raise ValueError(msg)
        if target.endswith("apple-darwin"):
            for path in sorted(stage.rglob("*")):
                if path.is_file() and (path.suffix == ".dylib" or path.name == "codex-voice-host"):
                    subprocess.run(["codesign", "--verify", "--strict", str(path)], check=True)
        hashes = {p.relative_to(stage).as_posix(): checksum(p) for p in sorted(stage.rglob("*")) if p.is_file()}
        (stage / "onicode-runtime.json").write_text(
            json.dumps(
                {"protocol": 1, "buildCommit": BUILD, "sourceVersion": VERSION, "target": target, "sha256": hashes},
                indent=2,
            )
            + "\n"
        )
        if destination.exists():
            shutil.rmtree(destination)
        shutil.move(str(stage), destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--package", type=Path, help="explicit standalone Codex package directory")
    inputs.add_argument(
        "--download", action="store_true", help="download the checksum-pinned source package on the build machine"
    )
    parser.add_argument("--target", choices=PACKAGES, help="media target; defaults to the build machine")
    parser.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    target = args.target or host_target()
    if args.download:
        with tempfile.TemporaryDirectory(prefix="onicode-voice-package-") as temporary:
            package = download_package(Path(temporary), target)
            install(package, args.destination.resolve(), target)
    else:
        install(args.package.resolve(), args.destination.resolve(), target)
