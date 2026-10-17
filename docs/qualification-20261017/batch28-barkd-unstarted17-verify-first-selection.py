import json
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/next-upstream-barkd-unstarted17')
data=json.loads((root/'first-selection-list-fresh.json').read_text())
selected=[]
for suite in data['rust-suites'].values():
 for name,test in suite['testcases'].items():
  if test['filter-match']['status']=='matches' and not test.get('ignored',False): selected.append(name)
assert selected==['recovery::recovered_wallet_finds_mixed_origin_vtxos'], selected
(root/'fresh-listing-admission.json').write_text(json.dumps({'state':'LISTING_GATE_PASS','selected_unignored':selected,'native_tests_run':0})+'\n')
