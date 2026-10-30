#!/bin/sh
# Match an interactive shell's PATH and credentials, just like running wt there.
exec "${SHELL:-/bin/sh}" -ic 'WT_POPUP=1 exec "$HERDR_PLUGIN_ROOT/target/release/wt" tui'
