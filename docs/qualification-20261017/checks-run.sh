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
cd /home/francis/abandon-ship-fresh-integration/testing-track-worktree
exec just checks
