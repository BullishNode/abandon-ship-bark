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

all_selected=[]
for i,(expected,binary) in enumerate(zip(proof['expected_names_in_execution_order'],proof['test_binaries_in_execution_order']),1):
 command=['cargo-nextest','nextest','list','--binaries-metadata',proof['metadata_path'],'--message-format','json','--',expected,'--exact']
 result=subprocess.run(command,cwd='/home/francis/abandon-ship-fresh-integration',check=True,stdout=subprocess.PIPE)
 (root/f'case{i:02}'/'selection-list-fresh.json').write_bytes(result.stdout)
 data=json.loads(result.stdout);selected=[]
 for suite in data['rust-suites'].values():
  for name,test in suite['testcases'].items():
   if test['filter-match']['status']=='matches':
    assert not test.get('ignored',False), 'exactselected testmustnotbeignored'
    selected.append((suite['binary-id'],name))
 assert selected==[(binary,expected)],selected
 all_selected.extend(selected)
(root/'fresh-listing-admission.json').write_text(json.dumps({'state':'LISTING_GATE_PASS','selected_unignored':all_selected,'native_tests_run':0})+'\n')
