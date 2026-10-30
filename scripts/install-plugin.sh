#!/bin/sh
set -eu

# Keep the installed plugin independent of a task worktree's lifetime.
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
install_dir=${1:-${XDG_DATA_HOME:-"$HOME/.local/share"}/worktable/herdr-plugin}
session=${2:-}
case "$install_dir/" in
    /|"$source_dir/"|"$source_dir/"*|*/.claude/worktrees/*)
        printf '%s\n' 'Choose a permanent plugin directory outside the source checkout and task worktrees.' >&2
        exit 1
        ;;
    /*) ;;
    *) printf '%s\n' 'Plugin directory must be absolute.' >&2; exit 1 ;;
esac

mkdir -p "$install_dir/src" "$install_dir/scripts"
cp "$source_dir/Cargo.toml" "$source_dir/Cargo.lock" "$source_dir/herdr-plugin.toml" "$install_dir/"
cp -R "$source_dir/src/." "$install_dir/src/"
cp "$source_dir/scripts/popup.sh" "$install_dir/scripts/popup.sh"
cargo build --manifest-path "$install_dir/Cargo.toml" --target-dir "$install_dir/target" --release --locked

if [ -n "$session" ]; then
    "${HERDR_BIN_PATH:-herdr}" --session "$session" plugin link "$install_dir"
else
    "${HERDR_BIN_PATH:-herdr}" plugin link "$install_dir"
fi
