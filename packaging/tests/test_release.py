"""Exercise the release command against a local bare remote, without publishing."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "release.py"


class ReleaseTest(unittest.TestCase):
    def test_bump_tag_push_and_reject_reuse_or_dirty_tree(self):
        with tempfile.TemporaryDirectory(prefix="den-release-test-") as temporary:
            base = Path(temporary)
            repo, remote = base / "work", base / "remote.git"
            repo.mkdir()

            def run(*args, check=True):
                return subprocess.run(args, cwd=repo, text=True, capture_output=True, check=check)

            run("git", "init", "--bare", str(remote))
            run("git", "init", "-b", "main")
            run("git", "config", "user.name", "Release test")
            run("git", "config", "user.email", "release-test@example.invalid")
            run("git", "remote", "add", "origin", str(remote))
            (repo / "packaging").mkdir()
            shutil.copy2(SCRIPT, repo / "packaging/release.py")
            (repo / "crates/probe/src").mkdir(parents=True)
            (repo / "Cargo.toml").write_text('[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n[workspace.package]\nversion = "0.1.0"\n')
            (repo / "crates/probe/Cargo.toml").write_text('[package]\nname = "release-probe"\nversion.workspace = true\nedition = "2024"\n')
            (repo / "crates/probe/src/lib.rs").write_text("")
            run("cargo", "generate-lockfile", "--offline")
            run("git", "add", ".")
            run("git", "commit", "-m", "Initial")
            initial = run("git", "rev-parse", "HEAD").stdout
            command = (sys.executable, "packaging/release.py")
            run(*command, "0.1.1", "--dry-run")
            self.assertEqual(run("git", "rev-parse", "HEAD").stdout, initial)
            self.assertEqual(run("git", "status", "--porcelain").stdout, "")
            run(*command, "0.1.1")
            metadata = json.loads(run("cargo", "metadata", "--format-version", "1", "--no-deps", "--locked", "--offline").stdout)
            self.assertEqual(metadata["packages"][0]["version"], "0.1.1")
            self.assertIn('version = "0.1.1"', (repo / "Cargo.lock").read_text())
            self.assertEqual(run("git", "status", "--porcelain").stdout, "")
            head = run("git", "rev-parse", "HEAD").stdout.strip()
            self.assertEqual(run("git", "rev-list", "-n", "1", "v0.1.1").stdout.strip(), head)
            self.assertEqual(run("git", "ls-remote", "origin", "refs/heads/main").stdout.split()[0], head)
            self.assertNotEqual(run(*command, "0.1.1", check=False).returncode, 0)
            (repo / "uncommitted.txt").write_text("keep me")
            self.assertNotEqual(run(*command, "0.1.2", check=False).returncode, 0)
            self.assertEqual(run("git", "rev-parse", "HEAD").stdout.strip(), head)
            self.assertEqual((repo / "uncommitted.txt").read_text(), "keep me")


if __name__ == "__main__":
    unittest.main()
