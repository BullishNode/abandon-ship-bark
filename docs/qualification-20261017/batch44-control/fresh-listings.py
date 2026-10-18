import json,subprocess,os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/batch44-six-money-v9b')
p=json.loads((root/'selection-proof.json').read_text())
proof=p

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

allocation=json.loads((root/'root-allocation.json').read_text())
assert __import__('hashlib').sha256((root/'root-allocation.json').read_bytes()).hexdigest()==proof['root_authorization']['sha256']
assert allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert allocation['state']=='AUTHORIZED_EXTERNAL_V9B_SIX_REVIEWED_MONEY_CASES_AFTER_OWN_PREFLIGHT' and [[row['binary'],row['name']] for row in allocation['cases']]==[list(v) for v in zip(p['test_binaries_in_execution_order'],p['expected_names_in_execution_order'])]
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
