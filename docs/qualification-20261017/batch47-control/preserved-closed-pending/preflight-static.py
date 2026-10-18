import json, hashlib, sys, os, re, signal, subprocess, time
from pathlib import Path
from datetime import datetime, timezone

root = Path('/fresh-cache/testing-track-20261017/batch47-ordinary-recovery-three-v10')
proof = json.loads((root / 'selection-proof.json').read_text())
# Presence, including false/0/empty, enables the double drive: reject all presence.
if 'BARK_DOUBLE_DRIVE_ACTIONS' in os.environ:
    raise SystemExit('CLOSED: BARK_DOUBLE_DRIVE_ACTIONS must be fully unset after cached Nix')
assert proof['expected_names_in_execution_order'] == ['recovery::recovered_wallet_skips_exited_vtxo', 'recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit', 'recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub']
assert proof['test_binaries_in_execution_order'] == ['ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd']
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['threads']==1
assert proof['maximum_case_executions']==3 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is False and proof['native_CLN_required_version'] is None
assert proof['expected_ignored'] is False and proof['run_ignored'] is None
if proof.get('candidate_ready') is not True or proof.get('controls_finalized') is not True or proof.get('execution_authorized') is not True:
    raise SystemExit('PENDING/UNADMITTED: candidate/allocation/actual environment unresolved; stop before resource/list/native')
# Deliberate independent hard stop: even flipping flags/removing the marker is insufficient.
raise SystemExit('CLOSED SCAFFOLD: actual root schema, all immutable candidate paths/hashes/versions/native CLN pins and parent fresh admission still require final integration and peer review')

assert not list(root.glob('case*/result.json')) and not (root/'cohort-progress.json').exists(), 'allocated case must not be retried'
assert not list(root.glob('case*/test-data')), 'fresh context required'
stat=os.statvfs('/dev/shm'); available=stat.f_bavail*stat.f_frsize
owned=[]
for p in Path('/proc').iterdir():
 if not p.name.isdigit(): continue
 try:
  if (p/'comm').read_text().strip()=='postgres' and '/fresh-cache/testing-track-20261017/' in (p/'cmdline').read_bytes().replace(b'\0',b' ').decode(errors='replace'): owned.append(int(p.name))
 except (FileNotFoundError,PermissionError,ProcessLookupError): pass
assert available>=12*1024*1024 and not owned, 'resource gate blocked; preserve all state and request root cleanup'
(root/'resource-admission.json').write_text(json.dumps({'state':'RESOURCE_GATE_PASS','shm_available_bytes':available,'live_owned_postgres':owned,'cleanup':False})+'\n')
