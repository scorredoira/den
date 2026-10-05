# Installing Den

Download a package from [Releases](https://github.com/scorredoira/den/releases).

| Platform | Package | Installation |
| --- | --- | --- |
| macOS 15+, Apple Silicon | `den-<version>-macos-aarch64.zip` | Unzip and drag `Den.app` to Applications. |
| macOS 15+, Intel | `den-<version>-macos-x86_64.zip` | Unzip and drag `Den.app` to Applications. |
| Linux x86_64, Ubuntu 24.04 or compatible | `den-<version>-linux-x86_64.tar.gz` | Extract and run `./install.sh`, or run `./den` directly. |
| Windows 10/11 x64 | `den-<version>-windows-x86_64.zip` | Extract the whole folder and open `den.exe`. Optional: run `install.ps1` for a per-user install and Start menu shortcut. |

**macOS first launch:** releases are ad hoc signed, without notarization. After trying to open Den, go to System Settings → Privacy & Security → Open Anyway. See [Apple's instructions](https://support.apple.com/102445). A normal download may be blocked until you authorize it.

**Linux:** a graphical Wayland or X11 session and a working Vulkan driver are required. On Ubuntu 24.04, install runtime dependencies with `sudo apt install libfontconfig1 libwayland-client0 libwebkit2gtk-4.1-0 libxkbcommon-x11-0 libx11-xcb1 libssl3t64 libzstd1 libvulkan1`. The optional installer also needs Python 3. Packages built on Ubuntu 24.04 require its glibc baseline; they are not universal binaries for older distributions.

**Windows:** releases are unsigned, so Windows may show a publisher/SmartScreen warning. Install Git for Windows and the Windows OpenSSH client and make `git` and `ssh` available on PATH. The default terminal is PowerShell (PowerShell 7 when installed). Keep the bundled agent next to `den.exe`. Close the app before replacing an installed release. To work inside WSL, add the distro as a server (`wsl:<distro>`, also listed in the server picker): Den installs its Linux agent there and runs files, git, language servers and terminals inside the distro, which it keeps running while the agent has terminals.

Git must be installed on every machine where you use repositories. SSH connections currently support Linux x86_64 servers; every desktop package includes their static agent. Install Claude Code and any language servers you use separately.

Every release includes `SHA256SUMS`. On macOS use `shasum -a 256 <archive>`; on Linux use `sha256sum <archive>`; on Windows use `Get-FileHash <archive> -Algorithm SHA256`.

To build from source, install Rust and the native dependencies from the [GPUI Kit installation guide](https://gpui-kit.com/docs/installation/), then run:

```sh
git clone https://github.com/scorredoira/den && cd den
cargo build --release --locked -p ui -p agent
```

Both executables are in `target/release`. On macOS, `./install` builds and installs `Den.app`; `./run` opens a development build. To include the Linux SSH agent when building on macOS, install `cargo-zigbuild` and Zig first.
