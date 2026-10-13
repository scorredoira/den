# Publishing releases

CI tests every crate on macOS Apple Silicon, macOS Intel, Linux and Windows on every branch push and pull request, and checks the code with Clippy and the dependencies with `cargo audit` (allowed advisories are in `.cargo/audit.toml`). Pushes to master and manual runs (**Actions → CI → Run workflow**) also build the release packages, which can be downloaded from the successful workflow's artifacts before publishing. No signing certificates are needed; the one repository secret is the release key (see below).

The Rust version is pinned in `rust-toolchain.toml` and, for CI, in `RUST_TOOLCHAIN` in `.github/workflows/build.yml`; change both together. Actions are pinned to commits, which Dependabot keeps up to date along with the crates (except the vendored GPUI ones).

Once your changes are committed, publish a new version from the repository root:

```sh
./release 0.1.1
# Windows: python packaging/release.py 0.1.1
# Preview without changes: ./release 0.1.1 --dry-run
```

This updates the workspace version and lockfile, creates a version commit and an annotated tag, and pushes the branch and tag together. The `Release` workflow runs the tests and publishes all four packages with checksums and generated release notes only after every build succeeds. Use a version such as `0.2.0-beta.1` for a prerelease. Publishing takes several minutes; the first build is slower while caches fill.

Alternatively, update `Cargo.toml` and `Cargo.lock`, commit and push, then open **Actions → Release → Run workflow** on that branch. It publishes the version in `Cargo.toml` and creates its tag. A manually pushed `v<version>` tag also triggers publication; the tag must match the manifest. Existing releases are never overwritten. Uploads are assembled in a draft before becoming public; if an upload fails, delete that incomplete draft before rerunning the publication. If a build fails before publication, fix the failure and use a new version, or rerun a transient failure on the same commit.

macOS packages use ad hoc signing and Windows packages are unsigned. GUI behavior and installation should also be checked on real machines; CI tests the code and builds packages but does not exercise a real desktop session.

## The release key

The `Release` workflow signs `SHA256SUMS` with [minisign](https://jedisct1.github.io/minisign/), and installed apps update only to a release whose `SHA256SUMS` is signed with that key for that release. Its public half is `packaging/minisign.pub`, built into the app; the secret half is the repository secret `MINISIGN_SECRET_KEY`, and without it the workflow stops before publishing.

Keep a copy of the secret key outside GitHub (secrets can't be read back). Changing it means a new `packaging/minisign.pub`, and the apps already installed, which trust the old key, then refuse updates until they are reinstalled by hand: replace it only if it leaks.

To check a download by hand:

```sh
minisign -V -p packaging/minisign.pub -m SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
```

## Pruning target/

Every release changes the workspace version, which gives each of Den's crates new artifacts without removing the old ones, so `target/` grows with each release built locally. [cargo-sweep](https://github.com/holmgr/cargo-sweep) removes what recent builds didn't use:

```sh
cargo install cargo-sweep
cargo sweep --time 14     # artifacts not used in the last 14 days
cargo sweep --installed   # artifacts of toolchains no longer installed
```

`cargo clean` removes everything; the next build is a full one.
