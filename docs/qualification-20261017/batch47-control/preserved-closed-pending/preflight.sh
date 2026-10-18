#!/usr/bin/env bash
set -euo pipefail
umask 077
# Cached Nix supplies guard Python; never invoke Python first (batch45 caller127).
source /fresh-cache/default-env.sh >/dev/null
# Presence alone enables double drive: false/0/empty are not absence.
unset BARK_DOUBLE_DRIVE_ACTIONS
unset LIGHTNINGD_DOCKER_IMAGE TEST_POSTGRES_HOST USE_FILESTORE OLD_BARK_EXEC OLD_CAPTAIND_EXEC
# No guessed product/CLN/SQLite/Postgres slots or PATH overrides. This gate is CLOSED.
exec python3 /fresh-cache/testing-track-20261017/batch47-ordinary-recovery-three-v10/preflight-static.py
