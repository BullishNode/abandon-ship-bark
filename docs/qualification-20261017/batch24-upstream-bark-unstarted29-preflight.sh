#!/usr/bin/env bash
set -euo pipefail
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export NEXTEST_USER_CONFIG_FILE=none
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
sha256sum -c /fresh-cache/testing-track-20261017/batch24-upstream-bark-unstarted29/runtime-pins.sha256
python3 /fresh-cache/testing-track-20261017/batch24-upstream-bark-unstarted29/preflight-static.py
cargo nextest list --binaries-metadata /fresh-cache/testing-track-20261017/batch24-upstream-bark-unstarted29/nextest-binaries-bark-runtime-immutable.json --message-format json -- mailbox::accept_mailbox --exact > /fresh-cache/testing-track-20261017/batch24-upstream-bark-unstarted29/first-selection-list-fresh.json
python3 /fresh-cache/testing-track-20261017/batch24-upstream-bark-unstarted29/verify-first-selection.py
