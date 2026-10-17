#!/usr/bin/env bash
set -euo pipefail
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export NEXTEST_USER_CONFIG_FILE=none
export LIGHTNINGD_EXEC=/nix/store/b7hmiq12w2g2x6dxn7w2gjgp9dc75s5q-clightning-26.06.6/bin/lightningd
export LIGHTNINGD_PLUGIN_DIR=/fresh-cache/cln-adapter-evidence/plugins
unset LIGHTNINGD_DOCKER_IMAGE
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
sha256sum -c /fresh-cache/testing-track-20261017/next-upstream-sdk-cln6/runtime-pins.sha256
python3 /fresh-cache/testing-track-20261017/next-upstream-sdk-cln6/preflight-static.py
cargo nextest list --binaries-metadata /fresh-cache/testing-track-20261017/next-upstream-sdk-cln6/nextest-binaries-sdk-runtime-immutable.json --message-format json -- lightning::receive_claim_on_mailbox_notification --exact > /fresh-cache/testing-track-20261017/next-upstream-sdk-cln6/first-selection-list-fresh.json
python3 /fresh-cache/testing-track-20261017/next-upstream-sdk-cln6/verify-first-selection.py
