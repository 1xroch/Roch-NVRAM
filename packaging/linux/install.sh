#!/bin/sh
# Installs Roch NVRAM for the current user: the binary to ~/.local/bin/roch-nvram, the launcher
# entry and its icon. Nothing runs as root here. Reading the BIOS setup database and writing settings
# ask for your password through pkexec; `sudo ~/.local/bin/roch-nvram setup` stops that. Works from
# the release tarball (everything beside this script) and from the repository after
# `cargo build --release --locked`; or pass the binary.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
if [ -x "$here/roch-nvram" ]; then default_bin="$here/roch-nvram"; else default_bin="$root/target/release/roch-nvram"; fi
if [ -f "$here/RochNVRAM.png" ]; then icon="$here/RochNVRAM.png"; else icon="$root/assets/roch_nvram.png"; fi
bin=${1:-"$default_bin"}
data=${XDG_DATA_HOME:-"$HOME/.local/share"}
install -Dm755 "$bin" "$HOME/.local/bin/roch-nvram"
sed "s|^Exec=.*|Exec=\"$HOME/.local/bin/roch-nvram\"|" "$here/RochNVRAM.desktop" > "$here/.RochNVRAM.desktop.tmp"
install -Dm644 "$here/.RochNVRAM.desktop.tmp" "$data/applications/RochNVRAM.desktop"
rm -f "$here/.RochNVRAM.desktop.tmp"
install -Dm644 "$icon" "$data/icons/hicolor/256x256/apps/RochNVRAM.png"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
echo "Installed: $HOME/.local/bin/roch-nvram (launcher: Roch NVRAM)"
if [ -e /usr/local/bin/roch-nvram ]; then
  echo "Run 'sudo $HOME/.local/bin/roch-nvram setup' again so the password-free helper matches this version."
else
  echo "Optional: 'sudo $HOME/.local/bin/roch-nvram setup' stops the password prompts (see the README)."
fi
