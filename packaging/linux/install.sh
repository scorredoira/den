#!/bin/sh
# Install this unpacked release for the current user; no root required.
set -eu
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}
app_dir="$data_dir/sik/app"
mkdir -p "$app_dir" "$HOME/.local/bin" "$data_dir/applications" "$data_dir/icons/hicolor/scalable/apps"
# Replace executables through rename, allowing an old process to keep running.
for name in sik sik-agent sik-agent-linux-x86_64; do
    cp "$source_dir/$name" "$app_dir/$name.new"
    chmod +x "$app_dir/$name.new"
    mv -f "$app_dir/$name.new" "$app_dir/$name"
done
ln -sfn "$app_dir/sik" "$HOME/.local/bin/sik"
cp "$source_dir/sik.svg" "$data_dir/icons/hicolor/scalable/apps/sik.svg"
# Desktop launchers do not necessarily inherit ~/.local/bin in PATH.
python3 - "$source_dir/sik.desktop" "$data_dir/applications/sik.desktop" "$app_dir/sik" <<'PY'
import pathlib, sys
source, target, executable = sys.argv[1:]
# Desktop Exec quoting: backslash, quote, dollar and backtick are reserved.
for char in ('\\', '"', '$', '`'):
    executable = executable.replace(char, '\\' + char)
executable = executable.replace('%', '%%')
pathlib.Path(target).write_text(pathlib.Path(source).read_text().replace('Exec=sik %F', f'Exec="{executable}" %F'))
PY
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data_dir/applications" || true
fi
printf 'Installed Sik. Open it from your applications menu or run %s/.local/bin/sik\n' "$HOME"
