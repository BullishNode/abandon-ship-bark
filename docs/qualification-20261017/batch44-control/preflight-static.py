import json, os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/batch44-six-money-v9b')
proof=json.loads((root/'selection-proof.json').read_text())

assert proof.get('candidate_ready') is True, 'exact v9b candidate pins pending; no listing/preflight/native authorized'
assert proof.get('execution_authorized') is True and proof['state']=='ROOT_ALLOCATED_ONCE_PENDING_PARENT_LAUNCH', 'explicit exact v9b root allocation pending'
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==6
assert len(set(proof['expected_names_in_execution_order']))==6
assert proof['expected_names_in_execution_order']==['qualification_conflict_reorg::expiry_actual_partial_conflict_reorg_restores_one_canonical_payment', 'qualification_failed_swap::expiry_failed_swap_during_restart_pays_its_absent_owner_once', 'qualification_offboard_restart::registered_offboard_restarts_before_nursery_handoff_and_pays_absent_owner_once', 'lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds', 'exit::watchman_sweeps_funding_tx_during_exit', 'exit::watchman_sweeps_vtxo_chain_during_exit']
assert proof['test_binaries_in_execution_order']==['ark-testing::server', 'ark-testing::server', 'ark-testing::server', 'ark-testing::bark-sdk', 'ark-testing::bark-sdk', 'ark-testing::bark-sdk']
assert proof['test_binaries_in_execution_order'].count('ark-testing::server')==3 and proof['test_binaries_in_execution_order'].count('ark-testing::bark-sdk')==3
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['maximum_case_executions']==6
assert not proof['expected_ignored'] and proof['run_ignored'] is None
for key in ['source_manifest_sha256','bundle_manifest_sha256','runtime_metadata_sha256']:
    value=proof[key]
    assert isinstance(value,str) and len(value)==64 and all(c in '0123456789abcdef' for c in value), 'candidate hash pending'
assert isinstance(proof['metadata_path'],str) and proof['metadata_path'].startswith('/fresh-cache/') and 'PENDING' not in proof['metadata_path']

assert proof['expected_names_in_execution_order']==['qualification_conflict_reorg::expiry_actual_partial_conflict_reorg_restores_one_canonical_payment', 'qualification_failed_swap::expiry_failed_swap_during_restart_pays_its_absent_owner_once', 'qualification_offboard_restart::registered_offboard_restarts_before_nursery_handoff_and_pays_absent_owner_once', 'lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds', 'exit::watchman_sweeps_funding_tx_during_exit', 'exit::watchman_sweeps_vtxo_chain_during_exit']
allocation=json.loads((root/'root-allocation.json').read_text())
assert __import__('hashlib').sha256((root/'root-allocation.json').read_bytes()).hexdigest()==proof['root_authorization']['sha256']
assert allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert allocation['state']=='AUTHORIZED_EXTERNAL_V9B_SIX_REVIEWED_MONEY_CASES_AFTER_OWN_PREFLIGHT' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert [[row['binary'],row['name']] for row in allocation['cases']]==[list(pair) for pair in zip(proof['test_binaries_in_execution_order'],proof['expected_names_in_execution_order'])]
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
