# Publishing releases

CI builds, tests and packages macOS Apple Silicon, macOS Intel, Linux and Windows on every branch push and pull request. Packages can be downloaded from the successful workflow's artifacts before publishing. No signing certificates or extra repository secrets are needed.

Once your changes are committed, publish a new version from the repository root:

```sh
./release 0.1.1
# Windows: python packaging/release.py 0.1.1
# Preview without changes: ./release 0.1.1 --dry-run
```

This updates the workspace version and lockfile, creates a version commit and an annotated tag, and pushes the branch and tag together. The `Release` workflow runs the tests and publishes all four packages with checksums and generated release notes only after every build succeeds. Use a version such as `0.2.0-beta.1` for a prerelease. Publishing takes several minutes; the first build is slower while caches fill.

Alternatively, update `Cargo.toml` and `Cargo.lock`, commit and push, then open **Actions → Release → Run workflow** on that branch. It publishes the version in `Cargo.toml` and creates its tag. A manually pushed `v<version>` tag also triggers publication; the tag must match the manifest. Existing releases are never overwritten. Uploads are assembled in a draft before becoming public; if an upload fails, delete that incomplete draft before rerunning the publication. If a build fails before publication, fix the failure and use a new version, or rerun a transient failure on the same commit.

macOS packages use ad hoc signing and Windows packages are unsigned. GUI behavior and installation should also be checked on real machines; CI tests the code and builds packages but does not exercise a real desktop session.
