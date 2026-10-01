#!/usr/bin/env python3
"""Package already-built native binaries; never installs or publishes them."""
import argparse
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent


def version():
    return re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)[1]


def package(target, agent, output):
    binaries = ROOT / "target" / target / "release"
    platform = "macos" if "apple" in target else "windows" if "windows" in target else "linux"
    arch = target.split("-")[0]
    label = f"sik-{version()}-{platform}-{arch}"
    stage = ROOT / "target" / "packages" / label
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    output.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if platform == "windows" else ""
    if platform == "macos":
        app = stage / "Sik.app"
        bindir = app / "Contents" / "MacOS"
        resources = app / "Contents" / "Resources"
    else:
        bindir = resources = stage
    bindir.mkdir(parents=True, exist_ok=True)
    resources.mkdir(parents=True, exist_ok=True)
    for name in ("sik", "sik-agent"):
        shutil.copy2(binaries / (name + suffix), bindir)
    shutil.copy2(agent, resources / "sik-agent-linux-x86_64")
    shutil.copy2(ROOT / "LICENSE", resources)
    shutil.copy2(ROOT / "README.md", resources)
    # The vendored crates carry their original license notices.
    licenses = resources / "licenses"
    licenses.mkdir()
    for crate in (ROOT / "vendor").iterdir():
        if crate.is_dir():
            for notice in crate.glob("LICENSE*"):
                shutil.copy2(notice, licenses / f"{crate.name}-{notice.name}")
    shutil.copy2(ROOT / "crates/ui/assets/icons/LICENSE-LUCIDE", licenses)
    if platform == "macos":
        template = (ROOT / "packaging/macos/Info.plist").read_text()
        build = os.environ.get("GITHUB_RUN_NUMBER", "1")
        plist = plistlib.loads(template.replace("@VERSION@", version().split("-")[0]).replace("@BUILD@", build).encode())
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps(plist))
        shutil.copy2(ROOT / "packaging/macos/sik.icns", resources)
        # Ad hoc signing works without Apple credentials. This is deliberately
        # not a Developer ID/notarized distribution; README explains first open.
        for path in (bindir / "sik-agent", app):
            subprocess.run(["codesign", "--force", "--sign", "-", str(path)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
        archive = output / f"{label}.zip"
        archive.unlink(missing_ok=True)
        subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive)], check=True)
    elif platform == "linux":
        shutil.copy2(ROOT / "packaging/macos/sik.svg", stage)
        shutil.copy2(ROOT / "packaging/linux/sik.desktop", stage)
        shutil.copy2(ROOT / "packaging/linux/install.sh", stage)
        archive = output / f"{label}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(stage, arcname=label)
    else:
        shutil.copy2(ROOT / "packaging/windows/install.ps1", stage)
        archive = output / f"{label}.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zip_file:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    zip_file.write(path, Path(label) / path.relative_to(stage))
    print(archive)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--agent", type=Path, required=True, help="Linux x86_64 musl agent")
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    package(args.target, args.agent.resolve(), args.output.resolve())
