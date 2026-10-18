import json, os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8')
proof=json.loads((root/'selection-proof.json').read_text())

assert proof.get('candidate_ready') is True, 'exact v8 candidate pins pending; no listing/preflight/native authorized'
assert proof.get('execution_authorized') is True and proof['state']=='ROOT_ALLOCATED_ONCE_PENDING_PARENT_LAUNCH', 'explicit exact v8 root allocation pending'
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==12
assert len(set(proof['expected_names_in_execution_order']))==12
assert proof['expected_names_in_execution_order']==['onchain::list_utxos', 'onchain::onchain_drain', 'movement::board_board', 'movement::movement_offboard', 'movement::movement_send_onchain', 'offboard::bark_send_onchain', 'offboard::bark_rejects_offboarding_dust_amount', 'boards::board_all_barkd', 'recovery::recovered_wallet_finds_boarded_vtxo', 'recovery::recovered_wallet_finds_mixed_origin_vtxos', 'recovery::recovered_wallet_finds_round_vtxo', 'recovery::recovered_wallet_quarantines_invalid_spent_vtxos']
assert proof['test_binaries_in_execution_order']==['ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd']
assert proof['test_binaries_in_execution_order'].count('ark-testing::bark')==7 and proof['test_binaries_in_execution_order'].count('ark-testing::barkd')==5
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['maximum_case_executions']==12
assert not proof['expected_ignored'] and proof['run_ignored'] is None
for key in ['source_manifest_sha256','bundle_manifest_sha256','runtime_metadata_sha256']:
    value=proof[key]
    assert isinstance(value,str) and len(value)==64 and all(c in '0123456789abcdef' for c in value), 'candidate hash pending'
assert isinstance(proof['metadata_path'],str) and proof['metadata_path'].startswith('/fresh-cache/') and 'PENDING' not in proof['metadata_path']

assert proof['expected_names_in_execution_order']==['onchain::list_utxos', 'onchain::onchain_drain', 'movement::board_board', 'movement::movement_offboard', 'movement::movement_send_onchain', 'offboard::bark_send_onchain', 'offboard::bark_rejects_offboarding_dust_amount', 'boards::board_all_barkd', 'recovery::recovered_wallet_finds_boarded_vtxo', 'recovery::recovered_wallet_finds_mixed_origin_vtxos', 'recovery::recovered_wallet_finds_round_vtxo', 'recovery::recovered_wallet_quarantines_invalid_spent_vtxos']
allocation=json.loads((root/'root-allocation.json').read_text())
assert allocation['state']=='AUTHORIZED_EXTERNAL_ORDINARY12_SERIAL_LANE2_AFTER_OWN_PREFLIGHT' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert [[row['binary'],row['name']] for row in allocation['exact_cases']]==[list(pair) for pair in zip(proof['test_binaries_in_execution_order'],proof['expected_names_in_execution_order'])]
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
