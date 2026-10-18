import json, hashlib, sys, os, re, signal, subprocess, time, tarfile
from pathlib import Path
from datetime import datetime, timezone
root=Path('/fresh-cache/testing-track-20261017/batch49-default-daemon-singleton-v11c')
proof=json.loads((root/'selection-proof.json').read_text())
assert os.environ.get('IN_NIX_SHELL'), 'source cached Nix before guard Python'
for key in ['BARK_DOUBLE_DRIVE_ACTIONS','LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'fully unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['expected_names_in_execution_order']==['fallback_act_by::fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable'] and proof['test_binaries_in_execution_order']==['ark-testing::bark-sdk']
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==1
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['threads']==1 and proof['retries']==0
assert proof['maximum_case_executions']==1 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is False and proof['expected_ignored'] is False and proof['run_ignored'] is None
raw=(root/'root-allocation.json').read_bytes();assert hashlib.sha256(raw).hexdigest()=='0d4ccfaef628edf4495bb707c915282d1b79f8056622eae3f9771a4eac1ff900'==proof['root_authorization']['sha256']
allocation=json.loads(raw)
assert allocation['state']=='ALLOCATED_ONE_DAEMON_SINGLETON_AFTER_FINAL_GATES' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert allocation['binary']=='ark-testing::bark-sdk' and allocation['test']=='fallback_act_by::fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable' and allocation['runtime_container']=='abandon-fresh-runtime-20261017'
assert allocation['timeout_seconds']==180 and allocation['retries']==0 and allocation['serial']==1 and allocation['max_global_native']==2
assert allocation['requires_exact_CLN_and_both_plugins'] is False
assert allocation['source_file']==proof['source_file'] and allocation['source_file_sha256']==proof['source_file_sha256']
for actual,key in [('source_manifest_sha256','source_manifest_sha256'),('archive_sha256','source_archive_sha256'),('bundle_manifest_sha256','bundle_manifest_sha256'),('immutable_metadata_sha256','immutable_metadata_sha256'),('runtime_metadata_sha256','runtime_metadata_sha256'),('runtime_pins_sha256','runtime_pinlist_sha256')]:
    assert allocation[actual]==proof[key]
for relative,expected in proof['proof_sha256'].items():assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected,relative
proposal=json.loads((root/'source-proposal.json').read_text());peer=json.loads((root/'source-peer.json').read_text())
assert allocation['source_proposal_sha256']==peer['author_proposal_sha256']==proof['proof_sha256']['source-proposal.json']
assert allocation['peer_sha256']==proof['proof_sha256']['source-peer.json']
assert proposal['binary']=='ark-testing::bark-sdk' and proposal['name']=='fallback_act_by::fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable'
assert proposal['test_source']['function_sha256']=='f70ded222b148d44d06eadd1e4b541865820046a9a28c37e1af610b3ba0f88cb'
assert proposal['test_source']['brace_body_sha256']=='1e38f5ae82e6333b35b982a53439228f0f33fd36e3a8d824c66649cec7b6062d'
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
    source={item['archive_member']:archive.extractfile(item['archive_member']).read() for item in proposal['full_file_helper_pins']}
for item in proposal['full_file_helper_pins']:assert hashlib.sha256(source[item['archive_member']]).hexdigest()==item['sha256']
assert hashlib.sha256(source[proof['source_file']]).hexdigest()==proof['source_file_sha256']
for item in [proposal['test_source']]+proposal['named_helper_pins']:
    lines=source[item['file']].decode().splitlines()
    fragment='\n'.join(lines[item['start_line']-1:item['end_line']]).lstrip(' \t')
    assert hashlib.sha256(fragment.encode()).hexdigest()==item['function_sha256'],item['function']
    assert hashlib.sha256(fragment[fragment.index('{'):].encode()).hexdigest()==item['brace_body_sha256'],item['function']
metadata=json.loads(Path(proof['metadata_path']).read_text())
assert set(metadata['rust-binaries'])=={'ark-testing::bark','ark-testing::bark-sdk','ark-testing::barkd','ark-testing::server'}
assert metadata['rust-binaries']['ark-testing::bark-sdk']['binary-path']==base+'native-bark-sdk'
for key,value in proof['execution_environment'].items():assert os.environ.get(key)==value,key
assert os.environ.get('LD_LIBRARY_PATH','').split(':')[0]==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('NEXTEST_PROFILE')=='default' and os.environ.get('CHAIN_SOURCE')=='bitcoind'

assert not (root/'cohort-progress.json').exists() and not list(root.glob('case*/result.json')) and not list(root.glob('case*/test-data'))
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
