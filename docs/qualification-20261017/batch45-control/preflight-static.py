import json, os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/batch45-recovery-three-v10')
proof=json.loads((root/'selection-proof.json').read_text())

assert proof.get('candidate_ready') is True, 'PENDING/UNADMITTED: completed candidate handoff absent; stop before resource/list/native'
assert proof.get('controls_finalized') is True, 'PENDING: actual versions/env/control paths unresolved'
assert proof.get('execution_authorized') is True and proof['state']=='ROOT_ALLOCATED_ONCE_PENDING_PARENT_LAUNCH', 'PENDING: exact root allocation absent'
assert proof['expected_names_in_execution_order']==['recovery::recovered_wallet_finds_round_vtxo', 'recovery::recovered_wallet_finds_mixed_origin_vtxos', 'recovery::recovered_wallet_quarantines_invalid_spent_vtxos']
assert proof['test_binaries_in_execution_order']==['ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd']
assert len(set(proof['expected_names_in_execution_order']))==3 and len(proof['test_binaries_in_execution_order'])==3
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['threads']==1
assert proof['maximum_case_executions']==3 and proof['maximum_global_native']==2 and proof['CLN_required'] is False
assert not proof['expected_ignored'] and proof['run_ignored'] is None
for key in ['source_manifest_sha256','source_archive_sha256','bundle_manifest_sha256','runtime_metadata_sha256','immutable_metadata_sha256']:
    value=proof[key]
    assert isinstance(value,str) and len(value)==64 and all(c in '0123456789abcdef' for c in value), 'PENDING candidate hash'
assert isinstance(proof['metadata_path'],str) and proof['metadata_path'].startswith('/fresh-cache/') and 'PENDING' not in proof['metadata_path']
assert isinstance(proof['runtime_pin_count'],int) and proof['runtime_pin_count']>0, 'PENDING actual pinlist'
for value in proof['execution_environment'].values():
    assert isinstance(value,str) and value and 'PENDING' not in value, 'PENDING actual environment/version'
assert all(isinstance(proof['root_authorization'][k],str) and proof['root_authorization'][k] and 'PENDING' not in proof['root_authorization'][k] for k in ['state','cases_field','owner_thread','sha256','packet_path']), 'PENDING actual root packet schema/pin'

allocation=json.loads((root/'root-allocation.json').read_text())
assert __import__('hashlib').sha256((root/'root-allocation.json').read_bytes()).hexdigest()==proof['root_authorization']['sha256']
assert allocation['state']=='AUTHORIZED_EXTERNAL_THREE_CLIENT_CASES_AFTER_OWN_PREFLIGHT' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert allocation['binary']=='ark-testing::barkd' and allocation['cases']==proof['expected_names_in_execution_order']
assert proof['test_binaries_in_execution_order']==[allocation['binary']]*len(allocation['cases'])
assert allocation['source_manifest_sha256']==proof['source_manifest_sha256'] and allocation['archive_sha256']==proof['source_archive_sha256']
assert allocation['manifest_sha256']==proof['bundle_manifest_sha256'] and allocation['metadata_runtime_sha256']==proof['runtime_metadata_sha256'] and allocation['metadata_immutable_sha256']==proof['immutable_metadata_sha256']
assert allocation['pins_sha256']==__import__('hashlib').sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()
assert allocation['profile']=='default' and allocation['timeout_seconds']==180 and allocation['serial']==1 and allocation['retries']==0 and allocation['first_fail_stops'] is True and allocation['global_max_native']==2 and allocation['no_Cargo'] is True

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
