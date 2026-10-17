#!/usr/bin/env bash
set -euo pipefail
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export CAPTAIND_EXEC=/fresh-cache/runtime-s03-current-cut-correction-20261017/captaind
export WATCHMAND_EXEC=/fresh-cache/runtime-s03-current-cut-correction-20261017/watchmand
export BARK_EXEC=/fresh-cache/runtime-operation-client-20261017/bark
export BARKD_EXEC=/fresh-cache/runtime-operation-client-20261017/barkd
export TEST_DIRECTORY=/fresh-cache/testing-track-20261017/batch08-two-root-return-history/test-data
export KEEP_ALL_TEST_DATA=1
export NEXTEST_NO_TESTS=fail
cd /home/francis/abandon-ship-fresh-integration/testing-track-worktree
exec just --set JUSTFILE_DIR /fresh-cache/testing-track-20261017/batch08-two-root-return-history int-binaries /fresh-cache/testing-track-20261017/runtime-testing-e44-tests-v1/nextest-binaries-immutable.json 'expiry_frozen_recipient_is_paid_after_sender_change_refresh --test-threads 1 --retries 0 --config-file /fresh-cache/testing-track-20261017/batch08-two-root-return-history/nextest.toml'
