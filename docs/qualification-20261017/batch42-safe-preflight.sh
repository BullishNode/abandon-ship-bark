#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo CARGO_TARGET_DIR=/fresh-cache/target CARGO_NET_OFFLINE=true NEXTEST_USER_CONFIG_FILE=none
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
unset LIGHTNINGD_DOCKER_IMAGE TEST_POSTGRES_HOST
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
python3 /fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8/preflight-static.py
sha256sum -c /fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8/runtime-pins.sha256
test "$(/fresh-cache/runtime-reorg-admission-v8e-20261017/bark --version)" = "bark 0.7.1-dev (f1a609d9b44d0a7b7513d7d162e6b5e71f5a61f0)"
test "$(/fresh-cache/runtime-reorg-admission-v8e-20261017/barkd --version)" = "barkd 0.7.1-dev (f1a609d9b44d0a7b7513d7d162e6b5e71f5a61f0)"
test "$(/fresh-cache/runtime-reorg-admission-v8e-20261017/captaind --version)" = "captaind 0.7.1-dev+f1a609d9b44d0a7b7513d7d162e6b5e71f5a61f0"
test "$(/fresh-cache/runtime-reorg-admission-v8e-20261017/watchmand --version)" = "watchmand 0.7.1-dev+f1a609d9b44d0a7b7513d7d162e6b5e71f5a61f0"
python3 /fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8/fresh-listings.py
