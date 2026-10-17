#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo CARGO_TARGET_DIR=/fresh-cache/target CARGO_NET_OFFLINE=true NEXTEST_USER_CONFIG_FILE=none
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
unset LIGHTNINGD_DOCKER_IMAGE TEST_POSTGRES_HOST
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
sha256sum -c /fresh-cache/testing-track-20261017/batch41-live-funded-v7/runtime-pins.sha256
test "$(/fresh-cache/runtime-owner-funded-v7-20261017/bark --version)" = "bark 0.7.1-dev (759f12c1f0c7586c597557f422374415f330b9f8)"
test "$(/fresh-cache/runtime-owner-funded-v7-20261017/barkd --version)" = "barkd 0.7.1-dev (759f12c1f0c7586c597557f422374415f330b9f8)"
test "$(/fresh-cache/runtime-owner-funded-v7-20261017/captaind --version)" = "captaind 0.7.1-dev+759f12c1f0c7586c597557f422374415f330b9f8"
test "$(/fresh-cache/runtime-owner-funded-v7-20261017/watchmand --version)" = "watchmand 0.7.1-dev+759f12c1f0c7586c597557f422374415f330b9f8"
python3 /fresh-cache/testing-track-20261017/batch41-live-funded-v7/preflight-static.py
python3 /fresh-cache/testing-track-20261017/batch41-live-funded-v7/fresh-listings.py
