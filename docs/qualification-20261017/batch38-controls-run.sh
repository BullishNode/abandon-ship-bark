#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export NEXTEST_USER_CONFIG_FILE=none
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
export CAPTAIND_EXEC=/fresh-cache/runtime-balance-backup-v5-20261017/captaind
export WATCHMAND_EXEC=/fresh-cache/runtime-balance-backup-v5-20261017/watchmand
export BARK_EXEC=/fresh-cache/runtime-balance-backup-v5-20261017/bark
export BARKD_EXEC=/fresh-cache/runtime-balance-backup-v5-20261017/barkd
export LD_LIBRARY_PATH=/nix/store/p774nkfb6frncliffdsyppg93n3cpb4b-sqlite-3.51.2/lib:${LD_LIBRARY_PATH:-}
export KEEP_ALL_TEST_DATA=1
export NEXTEST_NO_TESTS=fail
export NEXTEST_PROFILE=default
export CHAIN_SOURCE=bitcoind
unset USE_FILESTORE BARK_DOUBLE_DRIVE_ACTIONS OLD_BARK_EXEC OLD_CAPTAIND_EXEC TEST_POSTGRES_HOST
sha256sum -c /fresh-cache/testing-track-20261017/batch38-cold-backup-v5/runtime-pins.sha256 >/dev/null
test "$(/fresh-cache/runtime-balance-backup-v5-20261017/bark --version)" = "bark 0.7.1-dev (9ea05f9680be2757e875ee81f4546fd24e5b6354)"
exec python3 /fresh-cache/testing-track-20261017/batch38-cold-backup-v5/server-cohort.py
