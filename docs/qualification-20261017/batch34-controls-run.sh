#!/usr/bin/env bash
set -euo pipefail
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export CAPTAIND_EXEC=/fresh-cache/runtime-owner-scan-split-v1-20261017/captaind
export WATCHMAND_EXEC=/fresh-cache/runtime-owner-scan-split-v1-20261017/watchmand
export BARK_EXEC=/fresh-cache/runtime-owner-scan-split-v1-20261017/bark
export BARKD_EXEC=/fresh-cache/runtime-owner-scan-split-v1-20261017/barkd
export LIGHTNINGD_PLUGIN_DIR=/fresh-cache/cln-adapter-evidence/plugins
export LD_LIBRARY_PATH=/nix/store/p774nkfb6frncliffdsyppg93n3cpb4b-sqlite-3.51.2/lib:${LD_LIBRARY_PATH:-}
export KEEP_ALL_TEST_DATA=1
export NEXTEST_NO_TESTS=fail
export NEXTEST_PROFILE=default
sha256sum -c /fresh-cache/testing-track-20261017/next-upstream-bark-unstarted12/runtime-pins.sha256 >/dev/null
export CHAIN_SOURCE=bitcoind
export NEXTEST_USER_CONFIG_FILE=none
unset USE_FILESTORE BARK_DOUBLE_DRIVE_ACTIONS OLD_BARK_EXEC OLD_CAPTAIND_EXEC TEST_POSTGRES_HOST
exec python3 /fresh-cache/testing-track-20261017/next-upstream-bark-unstarted12/bark-cohort.py
