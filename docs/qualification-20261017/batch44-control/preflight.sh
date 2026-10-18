#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo CARGO_TARGET_DIR=/fresh-cache/target CARGO_NET_OFFLINE=true NEXTEST_USER_CONFIG_FILE=none
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
export LIGHTNINGD_EXEC=/nix/store/b7hmiq12w2g2x6dxn7w2gjgp9dc75s5q-clightning-26.06.6/bin/lightningd
export LIGHTNINGD_PLUGIN_DIR=/fresh-cache/cln-adapter-evidence/plugins
unset LIGHTNINGD_DOCKER_IMAGE USE_FILESTORE BARK_DOUBLE_DRIVE_ACTIONS OLD_BARK_EXEC OLD_CAPTAIND_EXEC TEST_POSTGRES_HOST
export LD_LIBRARY_PATH=/nix/store/p774nkfb6frncliffdsyppg93n3cpb4b-sqlite-3.51.2/lib:${LD_LIBRARY_PATH:-}
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
python3 /fresh-cache/testing-track-20261017/batch44-six-money-v9b/preflight-static.py
sha256sum -c /fresh-cache/testing-track-20261017/batch44-six-money-v9b/runtime-pins.sha256
test "$(/fresh-cache/runtime-recovery-v9b-20261017/bark --version)" = "bark 0.7.1-dev (4fa8997ba21158f20326c19f68efba5f42af1f3c)"
test "$(/fresh-cache/runtime-recovery-v9b-20261017/barkd --version)" = "barkd 0.7.1-dev (4fa8997ba21158f20326c19f68efba5f42af1f3c)"
test "$(/fresh-cache/runtime-recovery-v9b-20261017/captaind --version)" = "captaind 0.7.1-dev+4fa8997ba21158f20326c19f68efba5f42af1f3c"
test "$(/fresh-cache/runtime-recovery-v9b-20261017/watchmand --version)" = "watchmand 0.7.1-dev+4fa8997ba21158f20326c19f68efba5f42af1f3c"
test "$(/nix/store/b7hmiq12w2g2x6dxn7w2gjgp9dc75s5q-clightning-26.06.6/bin/lightningd --version)" = "v26.06.6"
python3 /fresh-cache/testing-track-20261017/batch44-six-money-v9b/fresh-listings.py
