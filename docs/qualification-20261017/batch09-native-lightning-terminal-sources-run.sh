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
export TEST_DIRECTORY=/fresh-cache/testing-track-20261017/batch09-native-lightning-terminal-sources/test-data
export KEEP_ALL_TEST_DATA=1
export NEXTEST_NO_TESTS=fail
cd /home/francis/abandon-ship-fresh-integration
exec just --set JUSTFILE_DIR /fresh-cache/testing-track-20261017/batch09-native-lightning-terminal-sources int-binaries /fresh-cache/runtime-s03-current-cut-correction-20261017/nextest-binaries-immutable.json 'expiry_actual_failed_external_send_pays_offline_owner expiry_actual_no_part_external_send_pays_offline_owner expiry_unused_same_hash_grant_retains_its_own_liability expiry_unused_same_hash_grant_can_be_refunded_after_other_source_paid --test-threads 1 --retries 0 --config-file /fresh-cache/testing-track-20261017/batch09-native-lightning-terminal-sources/nextest.toml'
