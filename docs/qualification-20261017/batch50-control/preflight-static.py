import json, hashlib, sys, os, re, signal, subprocess, time, tarfile
from pathlib import Path
from datetime import datetime, timezone
root=Path('/fresh-cache/testing-track-20261017/batch50-H30-ML125-v17')
proof=json.loads((root/'selection-proof.json').read_text())
assert os.environ.get('IN_NIX_SHELL'), 'source cached Nix before Python'
for key in ['BARK_DOUBLE_DRIVE_ACTIONS','LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'fully unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['expected_names_in_execution_order']==['qualification_tiny_payout_grouping::three_sub_fee_leaves_pay_one_registered_owner_once', 'qualification_offboard_reply_restore::expiry_ml125_restored_offboard_intent_adopts_exact_original_discharge'] and proof['test_binaries_in_execution_order']==['ark-testing::server']*2
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==2
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['threads']==1 and proof['retries']==0
assert proof['maximum_case_executions']==2 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is False and proof['expected_ignored'] is False and proof['run_ignored'] is None
raw=(root/'root-allocation.json').read_bytes();assert hashlib.sha256(raw).hexdigest()=='423005848035ba599187a35a83fd342b523cddde52b2b1a088c0173824f96185'==proof['root_authorization']['sha256']
allocation=json.loads(raw)
assert allocation['state']=='CONCRETE_TWO_UNRUN_FIXTURES_NATIVE_ALLOCATION' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert [case['selector'] for case in allocation['cases']]==proof['expected_names_in_execution_order']
execution=allocation['execution']
assert execution['test_binary']=='ark-testing::server' and execution['serial']==1 and execution['max_global_native']==2
assert execution['timeout_seconds_per_case']==180 and execution['retries']==0 and execution['first_fail_stop'] is True and execution['native_case_limit']==2 and execution['profile']=='default'
for actual,key in [('source_manifest_sha256','source_manifest_sha256'),('source_archive_sha256','source_archive_sha256'),('bundle_manifest_sha256','bundle_manifest_sha256'),('immutable_metadata_sha256','immutable_metadata_sha256'),('runtime_metadata_sha256','runtime_metadata_sha256'),('runtime_pins_sha256','runtime_pinlist_sha256')]:assert allocation[actual]==proof[key]
for relative,expected in proof['proof_sha256'].items():assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected,relative
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==proof['runtime_pinlist_sha256']
pins={}
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    expected,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected,filename
    assert filename not in pins;pins[filename]=expected
assert len(pins)==allocation['runtime_pin_count']==proof['runtime_pin_count']==14
base='/fresh-cache/runtime-reserved-partial-v17-20261018/'
for filename,key in [('source-manifest.json','source_manifest_sha256'),('manifest.json','bundle_manifest_sha256'),('native-server','native_server_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:assert pins[base+filename]==proof[key]
for filename,key in [(proof['source_archive_path'],'source_archive_sha256'),(proof['immutable_metadata_path'],'immutable_metadata_sha256')]:
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==proof[key]
bindings=json.loads((root/'source-bindings.json').read_text())
assert bindings['allocation_sha256']==proof['root_authorization']['sha256']
assert [f['name'] for f in bindings['functions']]==proof['expected_names_in_execution_order']
with tarfile.open(proof['source_archive_path']) as archive:source={item['path']:archive.extractfile(item['path']).read() for item in bindings['full_files']}
for item in bindings['full_files']:assert hashlib.sha256(source[item['path']]).hexdigest()==item['captured_sha256'],item['path']
for case in allocation['cases']:assert hashlib.sha256(source[case['source']]).hexdigest()==case['source_sha256']
for item in bindings['functions']+bindings['helpers']:
    path=item.get('file',item.get('source_file'));lines=source[path].decode().splitlines()
    fragment='\n'.join(lines[item['start_line']-1:item['end_line']]).lstrip(' \t')
    assert hashlib.sha256(fragment.encode()).hexdigest()==item['function_sha256'],path
    assert hashlib.sha256(fragment[fragment.index('{'):].encode()).hexdigest()==item['brace_body_sha256'],path
assert hashlib.sha256(source['server/src/database/expiry_details.rs']).hexdigest()=='c11c205f88d36362a9c00e5049229531407f5687457898ff7395a19ae44683c6'
metadata=json.loads(Path(proof['metadata_path']).read_text())
assert set(metadata['rust-binaries'])=={'ark-testing::server'}
assert metadata['rust-binaries']['ark-testing::server']['binary-path']==base+'native-server'
immutable=json.loads(Path(proof['immutable_metadata_path']).read_text())
assert immutable['rust-binaries']==metadata['rust-binaries']
for key,value in proof['execution_environment'].items():assert os.environ.get(key)==value,key
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
disk=os.statvfs('/fresh-cache');disk_available=disk.f_bavail*disk.f_frsize
assert available>=256*1024*1024 and disk_available>=20*1024**3 and not owned, 'resource gate blocked; preserve all state and request root cleanup'
(root/'resource-admission.json').write_text(json.dumps({'state':'RESOURCE_GATE_PASS','shm_available_bytes':available,'live_owned_postgres':owned,'disk_available_bytes':disk_available,'cleanup':False})+'\n')
