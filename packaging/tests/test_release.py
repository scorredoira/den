"""Exercise the release command against a local bare remote, without publishing."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "release.py"
COMMAND = (sys.executable, "packaging/release.py")


class ReleaseTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="den-release-test-")
        self.addCleanup(temporary.cleanup)
        base = Path(temporary.name)
        self.repo, remote = base / "work", base / "remote.git"
        self.repo.mkdir()
        self.run_in_repo("git", "init", "--bare", str(remote))
        self.run_in_repo("git", "init", "-b", "master")
        self.run_in_repo("git", "config", "user.name", "Release test")
        self.run_in_repo("git", "config", "user.email", "release-test@example.invalid")
        self.run_in_repo("git", "remote", "add", "origin", str(remote))
        (self.repo / "packaging").mkdir()
        shutil.copy2(SCRIPT, self.repo / "packaging/release.py")
        (self.repo / "crates/probe/src").mkdir(parents=True)
        (self.repo / "Cargo.toml").write_text('[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n[workspace.package]\nversion = "0.1.0"\n')
        (self.repo / "crates/probe/Cargo.toml").write_text('[package]\nname = "release-probe"\nversion.workspace = true\nedition = "2024"\n')
        (self.repo / "crates/probe/src/lib.rs").write_text("")
        self.run_in_repo("cargo", "generate-lockfile", "--offline")
        self.run_in_repo("git", "add", ".")
        self.run_in_repo("git", "commit", "-m", "Initial")

    def run_in_repo(self, *args, check=True):
        return subprocess.run(args, cwd=self.repo, text=True, capture_output=True, check=check)

    def head(self):
        return self.run_in_repo("git", "rev-parse", "HEAD").stdout.strip()

    def test_bump_tag_push_and_reject_reuse_or_dirty_tree(self):
        run = self.run_in_repo
        initial = self.head()
        run(*COMMAND, "0.1.1", "--dry-run")
        self.assertEqual(self.head(), initial)
        self.assertEqual(run("git", "status", "--porcelain").stdout, "")
        run(*COMMAND, "0.1.1")
        metadata = json.loads(run("cargo", "metadata", "--format-version", "1", "--no-deps", "--locked", "--offline").stdout)
        self.assertEqual(metadata["packages"][0]["version"], "0.1.1")
        self.assertIn('version = "0.1.1"', (self.repo / "Cargo.lock").read_text())
        self.assertEqual(run("git", "status", "--porcelain").stdout, "")
        head = self.head()
        self.assertEqual(run("git", "rev-list", "-n", "1", "v0.1.1").stdout.strip(), head)
        self.assertEqual(run("git", "ls-remote", "origin", "refs/heads/master").stdout.split()[0], head)
        self.assertNotEqual(run(*COMMAND, "0.1.1", check=False).returncode, 0)
        (self.repo / "uncommitted.txt").write_text("keep me")
        self.assertNotEqual(run(*COMMAND, "0.1.2", check=False).returncode, 0)
        self.assertEqual(self.head(), head)
        self.assertEqual((self.repo / "uncommitted.txt").read_text(), "keep me")

    def test_only_master_unless_asked(self):
        run = self.run_in_repo
        initial = self.head()
        run("git", "switch", "-c", "feature")
        refused = run(*COMMAND, "0.1.1", check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("--any-branch", refused.stderr)
        self.assertEqual(self.head(), initial)
        self.assertEqual(run("git", "tag", "--list").stdout, "")
        run(*COMMAND, "0.1.1", "--any-branch")
        self.assertEqual(run("git", "ls-remote", "origin", "refs/heads/feature").stdout.split()[0], self.head())

    def test_a_failed_commit_restores_the_manifest_and_lockfile(self):
        run = self.run_in_repo
        initial = self.head()
        manifest, lockfile = (self.repo / "Cargo.toml").read_text(), (self.repo / "Cargo.lock").read_text()
        hook = self.repo / ".git/hooks/pre-commit"
        hook.write_text("#!/bin/sh\nexit 1\n")
        hook.chmod(0o755)
        self.assertNotEqual(run(*COMMAND, "0.1.1", check=False).returncode, 0)
        self.assertEqual(self.head(), initial)
        self.assertEqual((self.repo / "Cargo.toml").read_text(), manifest)
        self.assertEqual((self.repo / "Cargo.lock").read_text(), lockfile)
        self.assertEqual(run("git", "status", "--porcelain").stdout, "")
        self.assertEqual(run("git", "tag", "--list").stdout, "")


if __name__ == "__main__":
    unittest.main()
