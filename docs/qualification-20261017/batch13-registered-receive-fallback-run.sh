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
export LIGHTNINGD_PLUGIN_DIR=/fresh-cache/cln-adapter-evidence/plugins
export LD_LIBRARY_PATH=/nix/store/p774nkfb6frncliffdsyppg93n3cpb4b-sqlite-3.51.2/lib:${LD_LIBRARY_PATH:-}
export KEEP_ALL_TEST_DATA=1
export NEXTEST_NO_TESTS=fail
export TEST_DIRECTORY=/fresh-cache/testing-track-20261017/batch13-registered-receive-fallback/test-data
export NEXTEST_PROFILE=default
sha256sum -c /fresh-cache/testing-track-20261017/runtime-testing-direct-receive-v1/runtime-pins.sha256 >/dev/null
cd /home/francis/abandon-ship-fresh-integration
exec just --set JUSTFILE_DIR /fresh-cache/testing-track-20261017/batch13-registered-receive-fallback int-binaries /fresh-cache/testing-track-20261017/runtime-testing-direct-receive-v1/nextest-binaries-immutable.json 'qualification_registered_forwarded_receive_pays_absent_recipient_after_provider_stops --test-threads 1 --retries 0 --config-file /fresh-cache/testing-track-20261017/batch13-registered-receive-fallback/nextest.toml'
