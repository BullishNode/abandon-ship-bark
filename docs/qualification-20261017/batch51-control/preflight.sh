#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
unset BARK_DOUBLE_DRIVE_ACTIONS LIGHTNINGD_DOCKER_IMAGE TEST_POSTGRES_HOST USE_FILESTORE OLD_BARK_EXEC OLD_CAPTAIND_EXEC
export CARGO_HOME=/fresh-cache/cargo CARGO_TARGET_DIR=/fresh-cache/target CARGO_NET_OFFLINE=true NEXTEST_USER_CONFIG_FILE=none
export KEEP_ALL_TEST_DATA=1 NEXTEST_NO_TESTS=fail NEXTEST_PROFILE=default CHAIN_SOURCE=bitcoind
export BARK_EXEC=/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/bark
export BARKD_EXEC=/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/barkd
export CAPTAIND_EXEC=/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/captaind
export WATCHMAND_EXEC=/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/watchmand
export BARK_VERSION='bark 0.7.1-dev (57f59bf3bc1aaa97a765828726f56e54f69bc59b)'
export BARKD_VERSION='barkd 0.7.1-dev (57f59bf3bc1aaa97a765828726f56e54f69bc59b)'
export CAPTAIND_VERSION='captaind 0.7.1-dev+57f59bf3bc1aaa97a765828726f56e54f69bc59b'
export WATCHMAND_VERSION='watchmand 0.7.1-dev+57f59bf3bc1aaa97a765828726f56e54f69bc59b'
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
export SQLITE_LIBRARY_PATH=/nix/store/p774nkfb6frncliffdsyppg93n3cpb4b-sqlite-3.51.2/lib
export LD_LIBRARY_PATH="${SQLITE_LIBRARY_PATH}:${LD_LIBRARY_PATH:-}"
python3 /fresh-cache/testing-track-20261017/batch51-H32-H11-v19-v2/preflight-static.py
test "$(/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/bark --version)" = 'bark 0.7.1-dev (57f59bf3bc1aaa97a765828726f56e54f69bc59b)'
test "$(/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/barkd --version)" = 'barkd 0.7.1-dev (57f59bf3bc1aaa97a765828726f56e54f69bc59b)'
test "$(/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/captaind --version)" = 'captaind 0.7.1-dev+57f59bf3bc1aaa97a765828726f56e54f69bc59b'
test "$(/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018/watchmand --version)" = 'watchmand 0.7.1-dev+57f59bf3bc1aaa97a765828726f56e54f69bc59b'
python3 /fresh-cache/testing-track-20261017/batch51-H32-H11-v19-v2/fresh-listings.py
