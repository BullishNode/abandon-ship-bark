#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
unset BARK_DOUBLE_DRIVE_ACTIONS LIGHTNINGD_DOCKER_IMAGE TEST_POSTGRES_HOST USE_FILESTORE OLD_BARK_EXEC OLD_CAPTAIND_EXEC
export CARGO_HOME=/fresh-cache/cargo CARGO_TARGET_DIR=/fresh-cache/target CARGO_NET_OFFLINE=true NEXTEST_USER_CONFIG_FILE=none
export KEEP_ALL_TEST_DATA=1 NEXTEST_NO_TESTS=fail NEXTEST_PROFILE=default CHAIN_SOURCE=bitcoind
export BARK_EXEC=/fresh-cache/runtime-reserved-partial-v17-20261018/bark
export BARKD_EXEC=/fresh-cache/runtime-reserved-partial-v17-20261018/barkd
export CAPTAIND_EXEC=/fresh-cache/runtime-reserved-partial-v17-20261018/captaind
export WATCHMAND_EXEC=/fresh-cache/runtime-reserved-partial-v17-20261018/watchmand
export BARK_VERSION='bark 0.7.1-dev (d2fae7db778096ff940e3861ca575d0b7bcddb56)'
export BARKD_VERSION='barkd 0.7.1-dev (d2fae7db778096ff940e3861ca575d0b7bcddb56)'
export CAPTAIND_VERSION='captaind 0.7.1-dev+d2fae7db778096ff940e3861ca575d0b7bcddb56'
export WATCHMAND_VERSION='watchmand 0.7.1-dev+d2fae7db778096ff940e3861ca575d0b7bcddb56'
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
export SQLITE_LIBRARY_PATH=/nix/store/p774nkfb6frncliffdsyppg93n3cpb4b-sqlite-3.51.2/lib
export LD_LIBRARY_PATH="${SQLITE_LIBRARY_PATH}:${LD_LIBRARY_PATH:-}"
python3 /fresh-cache/testing-track-20261017/batch50-H30-ML125-v17/server-cohort.py --check-launch-authorization
test "$(/fresh-cache/runtime-reserved-partial-v17-20261018/bark --version)" = 'bark 0.7.1-dev (d2fae7db778096ff940e3861ca575d0b7bcddb56)'
test "$(/fresh-cache/runtime-reserved-partial-v17-20261018/barkd --version)" = 'barkd 0.7.1-dev (d2fae7db778096ff940e3861ca575d0b7bcddb56)'
test "$(/fresh-cache/runtime-reserved-partial-v17-20261018/captaind --version)" = 'captaind 0.7.1-dev+d2fae7db778096ff940e3861ca575d0b7bcddb56'
test "$(/fresh-cache/runtime-reserved-partial-v17-20261018/watchmand --version)" = 'watchmand 0.7.1-dev+d2fae7db778096ff940e3861ca575d0b7bcddb56'
exec python3 /fresh-cache/testing-track-20261017/batch50-H30-ML125-v17/server-cohort.py
