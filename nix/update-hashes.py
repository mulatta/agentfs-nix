#!/usr/bin/env python3
"""Update nix/hashes.json with correct hashes for agentfs Nix packages.

This script:
1. Prefetches the pyturso source hash from PyPI
2. Extracts Cargo.lock from the pyturso sdist and updates nix/pyturso-Cargo.lock
3. Calculates pyturso cargoOutputHashes via dummy-hash-and-build
4. Calculates TypeScript SDK npmDepsHash via dummy-hash-and-build

Can run from scratch (no existing hashes.json) — creates seed data automatically.
"""

import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).parent / "scripts"))

from updater.hash import DUMMY_SHA256_HASH, extract_hash_from_build_error
from updater.hashes_file import load_hashes, save_hashes
from updater.nix import NixCommandError, nix_build, nix_store_prefetch_file

REPO_ROOT = Path(__file__).parent.parent
HASHES_FILE = Path(__file__).parent / "hashes.json"
CARGO_LOCK_FILE = Path(__file__).parent / "pyturso-Cargo.lock"


def git_add(*paths: Path) -> None:
    """Stage files so Nix flake can see them (flakes only read git-tracked files)."""
    subprocess.run(
        ["git", "add", "--intent-to-add", *(str(p) for p in paths)],
        check=True,
        cwd=REPO_ROOT,
        capture_output=True,
    )

# Seed structure when hashes.json doesn't exist
SEED_DATA: dict[str, Any] = {
    "pyturso": {
        "version": "0.4.0rc17",
        "hash": DUMMY_SHA256_HASH,
        "cargoOutputHashes": {
            "syntect-5.2.0": DUMMY_SHA256_HASH,
        },
    },
    "typescriptSdk": {
        "npmDepsHash": DUMMY_SHA256_HASH,
    },
}


def pypi_sdist_url(pname: str, version: str) -> str:
    """Construct PyPI sdist URL."""
    return f"https://files.pythonhosted.org/packages/source/{pname[0]}/{pname}/{pname}-{version}.tar.gz"


def dummy_hash_build(
    package_attr: str,
    data: dict[str, Any],
    hash_path: list[str],
) -> str:
    """Calculate a hash via dummy-hash-and-build pattern.

    Writes DUMMY_SHA256_HASH at hash_path in data, saves hashes.json,
    runs nix build, extracts correct hash from error output.

    Args:
        package_attr: Nix package attribute (e.g., ".#agentfs-sdk-python")
        data: Full root hashes dict (mutated in place during build, restored on failure)
        hash_path: Key path to the hash value, e.g. ["typescriptSdk", "npmDepsHash"]

    Returns:
        Correct hash in SRI format

    """
    obj = data
    for key in hash_path[:-1]:
        obj = obj[key]
    leaf_key = hash_path[-1]

    original = obj[leaf_key]
    obj[leaf_key] = DUMMY_SHA256_HASH
    save_hashes(HASHES_FILE, data)

    try:
        nix_build(package_attr, check=True)
        msg = "Build succeeded with dummy hash — unexpected"
        raise ValueError(msg)
    except NixCommandError as e:
        new_hash = extract_hash_from_build_error(e.args[0])
        if not new_hash:
            obj[leaf_key] = original
            save_hashes(HASHES_FILE, data)
            msg = f"Could not extract hash from build error:\n{e.args[0]}"
            raise ValueError(msg) from e
        obj[leaf_key] = new_hash
        save_hashes(HASHES_FILE, data)
        return new_hash


def update_pyturso_source_hash(data: dict) -> None:
    """Update pyturso source hash from PyPI."""
    version = data["pyturso"]["version"]
    url = pypi_sdist_url("pyturso", version)
    print(f"Prefetching pyturso {version} from {url}...")
    new_hash = nix_store_prefetch_file(url)
    old_hash = data["pyturso"]["hash"]
    if new_hash != old_hash:
        print(f"  hash changed: {old_hash} -> {new_hash}")
    else:
        print(f"  hash unchanged")
    data["pyturso"]["hash"] = new_hash
    save_hashes(HASHES_FILE, data)


def update_pyturso_cargo_lock(data: dict) -> None:
    """Extract Cargo.lock from pyturso sdist and update local copy."""
    version = data["pyturso"]["version"]
    url = pypi_sdist_url("pyturso", version)
    print("Downloading pyturso sdist to extract Cargo.lock...")

    with tempfile.TemporaryDirectory() as tmpdir:
        sdist_path = Path(tmpdir) / "pyturso.tar.gz"
        urllib.request.urlretrieve(url, sdist_path)  # noqa: S310

        with tarfile.open(sdist_path, "r:gz") as tar:
            cargo_lock_member = None
            for member in tar.getmembers():
                if member.name.endswith("/Cargo.lock"):
                    cargo_lock_member = member
                    break

            if cargo_lock_member is None:
                print("  WARNING: No Cargo.lock found in sdist")
                return

            f = tar.extractfile(cargo_lock_member)
            if f is None:
                print("  WARNING: Could not extract Cargo.lock")
                return

            new_content = f.read().decode("utf-8")

    old_content = CARGO_LOCK_FILE.read_text() if CARGO_LOCK_FILE.exists() else ""
    if new_content != old_content:
        CARGO_LOCK_FILE.write_text(new_content)
        print(f"  Updated {CARGO_LOCK_FILE.name}")
    else:
        print(f"  {CARGO_LOCK_FILE.name} unchanged")


def update_pyturso_cargo_output_hashes(data: dict) -> None:
    """Update pyturso cargoOutputHashes via dummy-hash-and-build."""
    print("Calculating pyturso cargoOutputHashes...")
    for dep_name in list(data["pyturso"]["cargoOutputHashes"]):
        original = data["pyturso"]["cargoOutputHashes"][dep_name]
        new_hash = dummy_hash_build(
            ".#agentfs-sdk-python",
            data,
            ["pyturso", "cargoOutputHashes", dep_name],
        )
        if new_hash != original:
            print(f"  {dep_name}: {original} -> {new_hash}")
        else:
            print(f"  {dep_name}: unchanged")


def update_typescript_npm_deps_hash(data: dict) -> None:
    """Update TypeScript SDK npmDepsHash via dummy-hash-and-build."""
    print("Calculating TypeScript npmDepsHash...")
    original = data["typescriptSdk"]["npmDepsHash"]
    new_hash = dummy_hash_build(
        ".#agentfs-sdk-typescript",
        data,
        ["typescriptSdk", "npmDepsHash"],
    )
    if new_hash != original:
        print(f"  npmDepsHash: {original} -> {new_hash}")
    else:
        print(f"  npmDepsHash: unchanged")


def main() -> None:
    if HASHES_FILE.exists():
        print(f"Loading {HASHES_FILE}...")
        data = load_hashes(HASHES_FILE)
    else:
        print(f"{HASHES_FILE} not found, creating from seed data...")
        data = SEED_DATA
        save_hashes(HASHES_FILE, data)
        git_add(HASHES_FILE)

    update_pyturso_source_hash(data)
    update_pyturso_cargo_lock(data)
    git_add(HASHES_FILE, CARGO_LOCK_FILE)  # flakes require git-tracked files
    update_pyturso_cargo_output_hashes(data)
    update_typescript_npm_deps_hash(data)

    print("Done.")


if __name__ == "__main__":
    main()
