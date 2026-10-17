#!/usr/bin/env bash
set -euo pipefail
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_BUILD_JOBS=4
export CARGO_NET_OFFLINE=true
export BARK_WEB_DIST=/fresh-cache/web-dist-10ecfb9
cd /home/francis/abandon-ship-fresh-integration
exec cargo nextest list --package ark-testing --test server --features ark-testing/fallback-test-bip84 --list-type binaries-only --message-format json > /fresh-cache/testing-track-20261017/runtime-testing-canonical-lightning-v2/nextest-binaries-original.json
