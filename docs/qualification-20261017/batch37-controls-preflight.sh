#!/usr/bin/env bash
set -euo pipefail
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export NEXTEST_USER_CONFIG_FILE=none
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
sha256sum -c /fresh-cache/testing-track-20261017/next-upstream-server-reorg-retry1/runtime-pins.sha256
test "$(/fresh-cache/runtime-owner-scan-split-v1-20261017/bark --version)" = "bark 0.7.1-dev (814d7b0deb6a5f2a6758d34ca4313589f948f4e3)"
python3 /fresh-cache/testing-track-20261017/next-upstream-server-reorg-retry1/preflight-static.py
cargo nextest list --binaries-metadata /fresh-cache/testing-track-20261017/next-upstream-server-reorg-retry1/nextest-binaries-server-runtime-immutable.json --message-format json -- block_index::test_block_index_reorg_retry --exact > /fresh-cache/testing-track-20261017/next-upstream-server-reorg-retry1/first-selection-list-fresh.json
python3 /fresh-cache/testing-track-20261017/next-upstream-server-reorg-retry1/verify-first-selection.py
