import json, os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/next-upstream-bark-unstarted12')
proof=json.loads((root/'selection-proof.json').read_text())
assert len(proof['expected_names_in_execution_order'])==12
assert not list(root.glob('case*/result.json')), 'batch has started; do not treat preparation as fresh'
prior_root = Path('/fresh-cache/testing-track-20261017/next-upstream-bark-unstarted16')
prior_results = json.loads((prior_root / 'cohort-progress.json').read_text())
assert len(prior_results) == 4, 'continuation requires exact terminal prefix; never repeat started cases'
assert [x['name'] for x in prior_results] == ['offboard::bark_send_onchain_too_much', 'offboard::offboard_all', 'offboard::offboard_vtxos', 'onchain::onchain_drain']
assert [x['state'] for x in prior_results] == ['PASS', 'PASS', 'PASS', 'FAIL']
assert all(x.get('resource_error') is None for x in prior_results)
assert prior_results[-1]['exit_code'] == 100
assert len(list(prior_root.glob('case*/result.json'))) == 4

stat=os.statvfs('/dev/shm'); available=stat.f_bavail*stat.f_frsize
owned=[]
for p in Path('/proc').iterdir():
 if not p.name.isdigit(): continue
 try:
  if (p/'comm').read_text().strip()=='postgres' and '/fresh-cache/testing-track-20261017/' in (p/'cmdline').read_bytes().replace(b'\0',b' ').decode(errors='replace'): owned.append(int(p.name))
 except (FileNotFoundError,PermissionError,ProcessLookupError): pass
assert available>=12*1024*1024 and not owned, 'resource gate blocked; preserve evidence; request root cleanup'
(root/'resource-admission.json').write_text(json.dumps({'state':'RESOURCE_GATE_PASS','shm_available_bytes':available,'live_owned_postgres':owned,'cleanup':False})+'\n')
