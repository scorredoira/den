#!/usr/bin/env python3
"""Bump, commit and tag a release, then atomically push the branch and tag."""
import argparse
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="For example 0.1.1 or 0.2.0-beta.1")
    parser.add_argument("--dry-run", action="store_true", help="Validate and describe without changing anything")
    args = parser.parse_args()
    version = args.version.removeprefix("v")
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?", version):
        parser.error("use a version such as 0.1.1 or 0.2.0-beta.1")
    if git("status", "--porcelain"):
        parser.error("commit or stash your changes first; releases require a clean working tree")
    branch = git("branch", "--show-current")
    if not branch:
        parser.error("check out a branch before publishing")
    tag = f"v{version}"
    if git("tag", "--list", tag) or git("ls-remote", "--tags", "origin", f"refs/tags/{tag}"):
        parser.error(f"{tag} already exists; choose a new version")
    manifest = ROOT / "Cargo.toml"
    original = manifest.read_text()
    current = re.search(r'^version = "([^"]+)"', original, re.M)[1]
    if current == version:
        parser.error("choose a new version, or use Actions → Release → Run workflow for the current version")
    print(f"Release {current} → {version}: commit Cargo.toml/Cargo.lock on {branch}, tag {tag}, push to origin.", flush=True)
    if args.dry_run:
        return
    manifest.write_text(re.sub(r'^version = "[^"]+"', f'version = "{version}"', original, count=1, flags=re.M))
    subprocess.run(["cargo", "update", "--workspace", "--offline"], cwd=ROOT, check=True)
    subprocess.run(["git", "add", "Cargo.toml", "Cargo.lock"], cwd=ROOT, check=True)
    subprocess.run(["git", "commit", "-m", f"Release {tag}"], cwd=ROOT, check=True)
    subprocess.run(["git", "tag", "-a", tag, "-m", f"Sik {tag}"], cwd=ROOT, check=True)
    subprocess.run(["git", "push", "--atomic", "origin", f"HEAD:refs/heads/{branch}", f"refs/tags/{tag}"], cwd=ROOT, check=True)
    print("GitHub Actions will test, package and publish all platforms. See the repository's Actions tab.")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(f"Release stopped: {error}. Inspect git status and tags before retrying; nothing was reset.")
