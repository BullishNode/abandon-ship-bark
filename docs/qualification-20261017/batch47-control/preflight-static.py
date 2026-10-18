import json, hashlib, sys, os, re, signal, subprocess, time
from pathlib import Path
from datetime import datetime, timezone

root = Path('/fresh-cache/testing-track-20261017/batch47-ordinary-recovery-three-v10')
proof = json.loads((root/'selection-proof.json').read_text())
assert os.environ.get('IN_NIX_SHELL'), 'cached Nix must be sourced before Python'
assert 'BARK_DOUBLE_DRIVE_ACTIONS' not in os.environ, 'presence enables double drive; fully unset after cached Nix'
for key in ['LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['state']=='ROOT_ALLOCATED_ONCE_PENDING_PARENT_LAUNCH'
assert proof['expected_names_in_execution_order']==['recovery::recovered_wallet_skips_exited_vtxo', 'recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit', 'recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub'] and proof['test_binaries_in_execution_order']==['ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd']
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==3
assert len(set(proof['expected_names_in_execution_order']))==3
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['threads']==1
assert proof['maximum_case_executions']==3 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is False and proof['expected_ignored'] is False and proof['run_ignored'] is None
allocation_bytes=(root/'root-allocation.json').read_bytes()
assert hashlib.sha256(allocation_bytes).hexdigest()=='96a9f25bd6e64925b4a028f8e5f0cdbf32a893edb891a9aee39ff434917782fb'==proof['root_authorization']['sha256']
allocation=json.loads(allocation_bytes)
assert allocation['state']=='ALLOCATED_FINALIZE_PEER_PREFLIGHT_THEN_ONE_PARENT_LAUNCH'
assert allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a' and allocation['cohort']=='batch47-ordinary-recovery-three-v10'
assert allocation['container']=='abandon-fresh-runtime-20261017'
assert allocation['binary']=='ark-testing::barkd' and allocation['cases']==['recovery::recovered_wallet_skips_exited_vtxo', 'recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit', 'recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub']
assert allocation['source_manifest_sha256']==proof['source_manifest_sha256'] and allocation['archive_sha256']==proof['source_archive_sha256']
assert allocation['metadata_sha256']==proof['runtime_metadata_sha256']
assert allocation['source_peer_sha256']==proof['source_peer_sha256'] and allocation['immutable_source_binding_sha256']==proof['immutable_source_binding_sha256']
assert allocation['timeout_seconds_each']==180 and allocation['retries']==0 and allocation['serial']==1
assert allocation['max_global_native']==2 and allocation['stop_on_first_failure'] is True and allocation['CLN_required'] is False and allocation['no_Cargo_shared_target_write'] is True
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==allocation['pins_sha256']==proof['runtime_pinlist_sha256']
for relative,expected_hash in [('source-peer.json',allocation['source_peer_sha256']),('immutable-source-proof.json',allocation['immutable_source_binding_sha256'])]:
    assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected_hash
peer=json.loads((root/'source-peer.json').read_text());binding=json.loads((root/'immutable-source-proof.json').read_text())
assert peer['proposal']['json_sha256']==binding['proposal']['sha256']=='9ae492d696dba9d010ca4873f1cc3b8720197d2dd6429f42801dd138d0087c0d'
assert [case['literal'] for case in peer['cases']]==['recovery::recovered_wallet_skips_exited_vtxo', 'recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit', 'recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub']
assert all(case['binary']=='ark-testing::barkd' and case['whole_file_sha256']=='97118c62993592f80fc167a1346168389c0386dae88a762cc99205ee417567b4' for case in peer['cases'])
actual_pins=[]
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    if not line.strip(): continue
    expected_hash,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:
        assert hashlib.file_digest(stream,'sha256').hexdigest()==expected_hash, filename
    actual_pins.append((expected_hash,filename))
assert len(actual_pins)==proof['runtime_pin_count']==17
with Path(proof['source_archive_path']).open('rb') as stream:
    assert hashlib.file_digest(stream,'sha256').hexdigest()==proof['source_archive_sha256']
assert hashlib.sha256(Path(proof['metadata_path']).read_bytes()).hexdigest()==proof['runtime_metadata_sha256']
assert hashlib.sha256(Path(proof['immutable_metadata_path']).read_bytes()).hexdigest()==proof['immutable_metadata_sha256']
pin_map={filename:sha for sha,filename in actual_pins}
base='/fresh-cache/runtime-sponsor-terminal-v10-20261018/'
for filename,key in [('manifest.json','bundle_manifest_sha256'),('source-manifest.json','source_manifest_sha256'),('native-barkd','native_barkd_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:
    assert pin_map[base+filename]==proof[key]
assert all(os.environ.get(key)==value for key,value in proof['execution_environment'].items() if not key.endswith('_VERSION') and key!='SQLITE_LIBRARY_PATH')
assert os.environ.get('SQLITE_LIBRARY_PATH')==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('LD_LIBRARY_PATH','').split(':')[0]==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('NEXTEST_PROFILE')=='default' and os.environ.get('CHAIN_SOURCE')=='bitcoind'

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
