import json, hashlib, sys, os, re, signal, subprocess, time, tarfile
from pathlib import Path
from datetime import datetime, timezone
root=Path('/fresh-cache/testing-track-20261017/batch51-H32-H11-v19-v2')
proof=json.loads((root/'selection-proof.json').read_text())
assert hashlib.sha256(Path('/home/francis/abandon-ship-fresh-integration/justfile').read_bytes()).hexdigest()==proof['current_justfile_sha256'], 'current just recipe differs from captured V19'
assert os.environ.get('IN_NIX_SHELL'), 'source cached Nix before Python'
for key in ['BARK_DOUBLE_DRIVE_ACTIONS','LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'fully unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['expected_names_in_execution_order']==['qualification_dust_later_receipt::forfeited_small_receipt_does_not_revive_when_same_customer_receives_again', 'qualification_three_owner_forwarding::only_final_owner_receives_after_two_full_value_forwards'] and proof['test_binaries_in_execution_order']==['ark-testing::server']*2
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==2
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['threads']==1 and proof['retries']==0
assert proof['maximum_case_executions']==2 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is False and proof['expected_ignored'] is False and proof['run_ignored'] is None
assert hashlib.sha256((root/'root-preparation.json').read_bytes()).hexdigest()==proof['root_preparation_sha256']=='09dc9e68db34dee4e5e606c1a58a76a9775209a6cc108d714eaa779463c97b8d'
raw=(root/'root-allocation.json').read_bytes();assert hashlib.sha256(raw).hexdigest()==proof['root_authorization']['sha256']=='1df12af294e2871b863ea6a9f322ddfe9e1c63465e1c9d628d51722167ba7dca'
allocation=json.loads(raw)
assert allocation['state']=='EXECUTION_ALLOCATED_NOW' and allocation['cases']==proof['expected_names_in_execution_order']
assert allocation['container']=='abandon-fresh-runtime-20261017' and allocation['cwd']=='/home/francis/abandon-ship-fresh-integration'
budget=allocation['budget'];assert budget=={'per_case_seconds':180,'retries':0,'serial':1,'max_global_native':2,'cases':2}
legacy=allocation['root_legacy_result'];assert legacy['state']=='PASS' and legacy['own_scoped_actors']==legacy['unreadable_proc_entries']==0
assert legacy['formal_postflight_sha256']==proof['root_legacy_postflight_sha256']
assert hashlib.sha256(Path(proof['root_legacy_postflight_path']).read_bytes()).hexdigest()==proof['root_legacy_postflight_sha256']
for actual,key in [('source_manifest_sha256','source_manifest_sha256'),('bundle_manifest_sha256','bundle_manifest_sha256'),('metadata_immutable_sha256','immutable_metadata_sha256'),('metadata_runtime_sha256','runtime_metadata_sha256'),('runtime_pins_sha256','runtime_pinlist_sha256')]:assert allocation[actual]==proof[key]
assert allocation['metadata']==proof['metadata_path']
for relative,expected in proof['proof_sha256'].items():assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected,relative
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==proof['runtime_pinlist_sha256']
pins={}
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    expected,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected,filename
    assert filename not in pins;pins[filename]=expected
assert len(pins)==proof['runtime_pin_count']==16
base='/fresh-cache/runtime-extra-money-fixtures-v19-20261018/'
for filename,key in [('source-manifest.json','source_manifest_sha256'),('manifest.json','bundle_manifest_sha256'),('native-server','native_server_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:assert pins[base+filename]==proof[key]
manifest=json.loads(Path(base+'manifest.json').read_text())
for product,expected in proof['products'].items():
    assert manifest['products'][product]==expected
    executable=os.environ.get(product.upper()+'_EXEC');assert executable==expected['path']
    assert pins[executable]==expected['sha256']
    with Path(executable).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected['sha256']
for filename,key in [(proof['source_archive_path'],'source_archive_sha256'),(proof['immutable_metadata_path'],'immutable_metadata_sha256')]:
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==proof[key]
sourceproof=json.loads((root/'source-artifact-proof.json').read_text());assert sourceproof['source_manifest_sha256']==proof['source_manifest_sha256'] and sourceproof['source_archive_sha256']==proof['source_archive_sha256']
assert [case['selector'] for case in sourceproof['our_source_bindings']]==proof['expected_names_in_execution_order']
filepins={path:record['captured_sha256'] for path,record in sourceproof['whole_file_API_schema_helper_bindings'].items()}
for case in sourceproof['our_source_bindings']:filepins[case['file']]=case['full_file_sha256']
with tarfile.open(proof['source_archive_path']) as archive:source={path:archive.extractfile(path).read() for path in filepins}
for path,expected in filepins.items():assert hashlib.sha256(source[path]).hexdigest()==expected,path
for case in sourceproof['our_source_bindings']:
    assert case['explicit_module'] in source['testing/tests/server/mod.rs'].decode()
    for item in [case['entry_function']]+case['local_helpers_and_implementation_regions']:
        lines=source[item['file']].decode().splitlines();fragment='\n'.join(lines[item['start_line']-1:item['end_line']]).lstrip(' \t')
        assert hashlib.sha256(fragment.encode()).hexdigest()==item['function_sha256']
        assert hashlib.sha256(fragment[fragment.index('{'):].encode()).hexdigest()==item['brace_body_sha256']
metadata=json.loads(Path(proof['metadata_path']).read_text());immutable=json.loads(Path(proof['immutable_metadata_path']).read_text())
assert set(metadata['rust-binaries'])=={'ark-testing::server'}
assert metadata['rust-binaries']==immutable['rust-binaries'] and metadata['rust-binaries']['ark-testing::server']['binary-path']==base+'native-server'
a=immutable['rust-build-meta']['non-test-binaries'];b=metadata['rust-build-meta']['non-test-binaries'];assert a.keys()==b.keys();changes=0
for package,entries in a.items():
    assert len(entries)==len(b[package])
    for old,new in zip(entries,b[package]):
        assert {k:v for k,v in old.items() if k!='path'}=={k:v for k,v in new.items() if k!='path'}
        assert new['path']=='../runtime-extra-money-fixtures-v19-20261018/support/'+old['path']
        changes+=old['path']!=new['path']
assert changes==3
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
 assert len(data['rust-suites'])==1, 'V19 has one native binary'
 for suite in data['rust-suites'].values():
  for name,test in suite['testcases'].items():
   if test['filter-match']['status']=='matches':
    assert not test.get('ignored',False), 'exactselected testmustnotbeignored'
    selected.append((suite['binary-id'],name))
 assert selected==[(binary,expected)],selected
 all_selected.extend(selected)
(root/'fresh-listing-admission.json').write_text(json.dumps({'state':'LISTING_GATE_PASS','selected_unignored':all_selected,'native_tests_run':0})+'\n')
