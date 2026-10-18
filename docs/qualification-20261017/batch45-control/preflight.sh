#!/usr/bin/env bash
set -euo pipefail
umask 077
python3 /fresh-cache/testing-track-20261017/batch45-recovery-three-v10/preflight-static.py
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo CARGO_TARGET_DIR=/fresh-cache/target CARGO_NET_OFFLINE=true NEXTEST_USER_CONFIG_FILE=none
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
unset LIGHTNINGD_DOCKER_IMAGE TEST_POSTGRES_HOST
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
sha256sum -c /fresh-cache/testing-track-20261017/batch45-recovery-three-v10/runtime-pins.sha256
test "$(/fresh-cache/runtime-sponsor-terminal-v10-20261018/bark --version)" = "bark 0.7.1-dev (783c0c5c51105b1ec1a5fa1068820ce94e18017e)"
test "$(/fresh-cache/runtime-sponsor-terminal-v10-20261018/barkd --version)" = "barkd 0.7.1-dev (783c0c5c51105b1ec1a5fa1068820ce94e18017e)"
test "$(/fresh-cache/runtime-sponsor-terminal-v10-20261018/captaind --version)" = "captaind 0.7.1-dev+783c0c5c51105b1ec1a5fa1068820ce94e18017e"
test "$(/fresh-cache/runtime-sponsor-terminal-v10-20261018/watchmand --version)" = "watchmand 0.7.1-dev+783c0c5c51105b1ec1a5fa1068820ce94e18017e"
python3 /fresh-cache/testing-track-20261017/batch45-recovery-three-v10/fresh-listings.py
