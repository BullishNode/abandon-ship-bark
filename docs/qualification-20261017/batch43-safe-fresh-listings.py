import json,subprocess,os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/batch43-quarantine-v8e')
p=json.loads((root/'selection-proof.json').read_text())
proof=p

assert proof.get('candidate_ready') is True, 'exact v8 candidate pins pending; no listing/preflight/native authorized'
assert proof.get('execution_authorized') is True and proof['state']=='ROOT_ALLOCATED_ONCE_PENDING_PARENT_LAUNCH', 'explicit exact v8 root allocation pending'
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==1
assert len(set(proof['expected_names_in_execution_order']))==1
assert proof['expected_names_in_execution_order']==['recovery::recovered_wallet_quarantines_invalid_spent_vtxos']
assert proof['test_binaries_in_execution_order']==['ark-testing::barkd']
assert proof['test_binaries_in_execution_order'].count('ark-testing::bark')==0 and proof['test_binaries_in_execution_order'].count('ark-testing::barkd')==1
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['maximum_case_executions']==1
assert not proof['expected_ignored'] and proof['run_ignored'] is None
for key in ['source_manifest_sha256','bundle_manifest_sha256','runtime_metadata_sha256']:
    value=proof[key]
    assert isinstance(value,str) and len(value)==64 and all(c in '0123456789abcdef' for c in value), 'candidate hash pending'
assert isinstance(proof['metadata_path'],str) and proof['metadata_path'].startswith('/fresh-cache/') and 'PENDING' not in proof['metadata_path']

allocation=json.loads((root/'root-allocation.json').read_text())
assert allocation['state']=='AUTHORIZED_SEPARATE_ONE_PREVIOUSLY_NOTRUN_QUARANTINE_CASE' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a' and [[row['binary'],row['name']] for row in allocation['exact_cases']]==[list(v) for v in zip(p['test_binaries_in_execution_order'],p['expected_names_in_execution_order'])]
all_selected=[]
for i,(expected,binary) in enumerate(zip(p['expected_names_in_execution_order'],p['test_binaries_in_execution_order']),1):
 command=['cargo-nextest','nextest','list','--binaries-metadata',p['metadata_path'],'--message-format','json','--',expected,'--exact']
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
