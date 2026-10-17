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
export TEST_DIRECTORY=/fresh-cache/testing-track-20261017/batch06-corrected-reorg-fee-conflict-split/test-data
export KEEP_ALL_TEST_DATA=1
export NEXTEST_NO_TESTS=fail
cd /home/francis/abandon-ship-fresh-integration
exec just --set JUSTFILE_DIR /fresh-cache/testing-track-20261017/batch06-corrected-reorg-fee-conflict-split int-binaries /fresh-cache/runtime-s03-current-cut-correction-20261017/nextest-binaries-immutable.json 'expiry_actual_payout_reorg_preserves_receipt_and_fence expiry_actual_deep_payout_reorg_preserves_receipt_and_fence expiry_payout_fee_replacement_keeps_parent_and_counts_only_confirmed_child expiry_actual_partial_exit_conflict_regroups_after_restart expiry_requested_split_preserves_mixed_siblings_and_eventual_refund --test-threads 1 --retries 0 --config-file /fresh-cache/testing-track-20261017/batch06-corrected-reorg-fee-conflict-split/nextest.toml'
