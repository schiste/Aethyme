#!/bin/sh
set -eu
handshake_dir=${AETHYME_FX10_GATE_HANDSHAKE_DIR:?FX10 gate handshake directory is required}
test -d "$handshake_dir"
printf '%s\n' "$$" >"$handshake_dir/started.pending"
mv "$handshake_dir/started.pending" "$handshake_dir/started"
heartbeat=0
while [ ! -f "$handshake_dir/release" ]; do
    heartbeat=$((heartbeat + 1))
    printf '%s\n' "$heartbeat" >"$handshake_dir/heartbeat.pending"
    mv "$handshake_dir/heartbeat.pending" "$handshake_dir/heartbeat"
    sleep 0.01
done
test -f app.sh
git diff --check HEAD^ HEAD
printf '%s\n' passed >"$handshake_dir/completed.pending"
mv "$handshake_dir/completed.pending" "$handshake_dir/completed"
