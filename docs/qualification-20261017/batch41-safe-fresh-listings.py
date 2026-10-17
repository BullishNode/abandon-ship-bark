import json,subprocess,os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/batch41-live-funded-v7')
p=json.loads((root/'selection-proof.json').read_text())
all_selected=[]
for i,(expected,binary) in enumerate(zip(p['expected_names_in_execution_order'],p['test_binaries_in_execution_order']),1):
 command=['cargo-nextest','nextest','list','--binaries-metadata',p['metadata_path'],'--message-format','json','--run-ignored','only','--',expected,'--exact']
 result=subprocess.run(command,cwd='/home/francis/abandon-ship-fresh-integration',check=True,stdout=subprocess.PIPE)
 (root/f'case{i:02}'/'selection-list-fresh.json').write_bytes(result.stdout)
 data=json.loads(result.stdout);selected=[]
 for suite in data['rust-suites'].values():
  for name,test in suite['testcases'].items():
   if test['filter-match']['status']=='matches':
    assert test.get('ignored',False), 'exactselected testmustbeexplicitlyignored'
    selected.append((suite['binary-id'],name))
 assert selected==[(binary,expected)],selected
 all_selected.extend(selected)
(root/'fresh-listing-admission.json').write_text(json.dumps({'state':'LISTING_IGNORED_ONLY_GATE_PASS','selected_ignored':all_selected,'native_tests_run':0})+'\n')
