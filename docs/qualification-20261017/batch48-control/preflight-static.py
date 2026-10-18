import json, hashlib, sys, os, re, signal, subprocess, time, tarfile
from pathlib import Path
from datetime import datetime, timezone
root=Path('/fresh-cache/testing-track-20261017/batch48-sdk-watch-bridge-v11c-allocated')
proof=json.loads((root/'selection-proof.json').read_text())
assert os.environ.get('IN_NIX_SHELL'), 'source cached Nix before guard Python'
for key in ['BARK_DOUBLE_DRIVE_ACTIONS','LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'fully unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['expected_names_in_execution_order']==['lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds', 'exit::watchman_sweeps_funding_tx_during_exit', 'exit::watchman_sweeps_vtxo_chain_during_exit'] and proof['test_binaries_in_execution_order']==['ark-testing::bark-sdk']*3
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==3
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['threads']==1 and proof['retries']==0
assert proof['maximum_case_executions']==3 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is True and proof['expected_ignored'] is False and proof['run_ignored'] is None
raw=(root/'root-allocation.json').read_bytes();assert hashlib.sha256(raw).hexdigest()=='27007c4235840bc86630eff49041b000434bbdbc84dc60cbd832e694c201f379'==proof['root_authorization']['sha256']
allocation=json.loads(raw)
assert allocation['state']=='ALLOCATED_THREE_REPEAT_SDK_BRIDGE_CASES_AFTER_FRESH_GATES' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert allocation['binary']=='ark-testing::bark-sdk' and allocation['tests']==proof['expected_names_in_execution_order'] and allocation['runtime_container']=='abandon-fresh-runtime-20261017'
assert allocation['timeout_seconds']==180 and allocation['retries']==0 and allocation['serial']==1 and allocation['max_global_native']==2
assert allocation['requires_exact_CLN_and_both_plugins'] is True
for actual,key in [('source_manifest_sha256','source_manifest_sha256'),('archive_sha256','source_archive_sha256'),('bundle_manifest_sha256','bundle_manifest_sha256'),('immutable_metadata_sha256','immutable_metadata_sha256'),('runtime_metadata_sha256','runtime_metadata_sha256'),('runtime_pins_sha256','runtime_pinlist_sha256')]:
    assert allocation[actual]==proof[key]
for relative,expected in proof['proof_sha256'].items():assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected,relative
proposal=json.loads((root/'source-proposal.json').read_text());peer=json.loads((root/'source-peer.json').read_text())
assert allocation['source_proposal_sha256']==peer['proposal_sha256']==proof['proof_sha256']['source-proposal.json']
assert allocation['peer_sha256']==proof['proof_sha256']['source-peer.json']
bindings=json.loads((root/'source-bindings.json').read_text())
assert [item['name'] for item in bindings['functions']]==allocation['tests']
assert bindings['proposal_sha256']==allocation['source_proposal_sha256'] and bindings['peer_sha256']==allocation['peer_sha256']
assert len(bindings['full_files'])==12 and len(bindings['functions'])==3
prior=json.loads((root/'prior49-postflight.json').read_text())
assert prior['state']==proof['prior49_postflight']['required_state'] and prior['parent_terminal_confirmed']['exit_code']==0
assert not prior['cleanup_resources']['remaining_group_or_SID_members'] and not prior['cleanup_resources']['remaining_scoped_argv_cwd_FD_actors'] and not prior['cleanup_resources']['unreadable_owned_proc_entries']
assert hashlib.sha256(Path(proof['saved_listing']['path']).read_bytes()).hexdigest()==proof['saved_listing']['sha256']
saved=json.loads(Path(proof['saved_listing']['path']).read_text())
for suite in saved['rust-suites'].values():
    if suite['binary-id']=='ark-testing::bark-sdk':
        for name in allocation['tests']:assert name in suite['testcases'] and not suite['testcases'][name].get('ignored',False)
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==proof['runtime_pinlist_sha256']
pins={}
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    expected,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected,filename
    assert filename not in pins;pins[filename]=expected
assert len(pins)==proof['runtime_pin_count']==17
base='/fresh-cache/runtime-terminal-replay-v11c-20261018/'
for filename,key in [('source-manifest.json','source_manifest_sha256'),('manifest.json','bundle_manifest_sha256'),('native-bark-sdk','native_sdk_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:assert pins[base+filename]==proof[key]
for filename,key in [(proof['source_archive_path'],'source_archive_sha256'),(proof['immutable_metadata_path'],'immutable_metadata_sha256')]:
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==proof[key]
with tarfile.open(proof['source_archive_path']) as archive:
    source={item['path']:archive.extractfile(item['path']).read() for item in bindings['full_files']}
for item in bindings['full_files']:assert hashlib.sha256(source[item['path']]).hexdigest()==item['C_sha256']
for item in bindings['functions']+bindings['helpers']:
    file=item.get('file',item.get('source_file')); lines=source[file].decode().splitlines()
    fragment='\n'.join(lines[item['start_line']-1:item['end_line']]).lstrip(' \t')
    assert hashlib.sha256(fragment.encode()).hexdigest()==item['function_sha256'],item.get('function',item.get('name'))
    assert hashlib.sha256(fragment[fragment.index('{'):].encode()).hexdigest()==item['brace_body_sha256'],item.get('function',item.get('name'))
metadata=json.loads(Path(proof['metadata_path']).read_text())
assert set(metadata['rust-binaries'])=={'ark-testing::bark','ark-testing::bark-sdk','ark-testing::barkd','ark-testing::server'}
assert metadata['rust-binaries']['ark-testing::bark-sdk']['binary-path']==base+'native-bark-sdk'
for key,value in proof['execution_environment'].items():assert os.environ.get(key)==value,key
assert os.environ.get('LD_LIBRARY_PATH','').split(':')[0]==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('NEXTEST_PROFILE')=='default' and os.environ.get('CHAIN_SOURCE')=='bitcoind'
assert proof['native_CLN_required_version']=='v26.06.6'

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
