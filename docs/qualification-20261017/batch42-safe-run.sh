#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export NEXTEST_USER_CONFIG_FILE=none
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
export CAPTAIND_EXEC=/fresh-cache/runtime-reorg-admission-v8e-20261017/captaind
export WATCHMAND_EXEC=/fresh-cache/runtime-reorg-admission-v8e-20261017/watchmand
export BARK_EXEC=/fresh-cache/runtime-reorg-admission-v8e-20261017/bark
export BARKD_EXEC=/fresh-cache/runtime-reorg-admission-v8e-20261017/barkd
export LD_LIBRARY_PATH=/nix/store/p774nkfb6frncliffdsyppg93n3cpb4b-sqlite-3.51.2/lib:${LD_LIBRARY_PATH:-}
export KEEP_ALL_TEST_DATA=1
export NEXTEST_NO_TESTS=fail
export NEXTEST_PROFILE=default
export CHAIN_SOURCE=bitcoind
unset LIGHTNINGD_DOCKER_IMAGE USE_FILESTORE BARK_DOUBLE_DRIVE_ACTIONS OLD_BARK_EXEC OLD_CAPTAIND_EXEC TEST_POSTGRES_HOST
python3 /fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8/server-cohort.py --check-launch-authorization
sha256sum -c /fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8/runtime-pins.sha256 >/dev/null
test "$(/fresh-cache/runtime-reorg-admission-v8e-20261017/bark --version)" = "bark 0.7.1-dev (f1a609d9b44d0a7b7513d7d162e6b5e71f5a61f0)"
exec python3 /fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8/server-cohort.py
