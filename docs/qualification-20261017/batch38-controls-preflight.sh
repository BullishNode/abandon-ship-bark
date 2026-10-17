#!/usr/bin/env bash
set -euo pipefail
umask 077
source /fresh-cache/default-env.sh >/dev/null
export CARGO_HOME=/fresh-cache/cargo
export CARGO_TARGET_DIR=/fresh-cache/target
export CARGO_NET_OFFLINE=true
export NEXTEST_USER_CONFIG_FILE=none
export POSTGRES_BINS=/nix/store/f5bx5vyjvfc52a93q6p7gy7aidwg3jpx-postgresql-17.11/bin
printf 'IN_NIX_SHELL=%s\n' "$IN_NIX_SHELL"
sha256sum -c /fresh-cache/testing-track-20261017/batch38-cold-backup-v5/runtime-pins.sha256
test "$(/fresh-cache/runtime-balance-backup-v5-20261017/bark --version)" = "bark 0.7.1-dev (9ea05f9680be2757e875ee81f4546fd24e5b6354)"
"$POSTGRES_BINS/pg_controldata" --version
python3 /fresh-cache/testing-track-20261017/batch38-cold-backup-v5/preflight-static.py
cargo-nextest nextest list --binaries-metadata /fresh-cache/runtime-balance-backup-v5-20261017/nextest-binaries-runtime.json --message-format json -- qualification_cold_backup::qualification_orderly_physical_backup_seed_only_recovery --exact > /fresh-cache/testing-track-20261017/batch38-cold-backup-v5/first-selection-list-fresh.json
python3 /fresh-cache/testing-track-20261017/batch38-cold-backup-v5/verify-first-selection.py
