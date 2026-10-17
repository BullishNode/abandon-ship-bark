import json, os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/batch41-live-funded-v7')
proof=json.loads((root/'selection-proof.json').read_text())
assert proof['expected_names_in_execution_order']==['expiry_owner_load::expiry_owner_live_funded_100_registrations', 'expiry_owner_load::expiry_owner_live_funded_1000_registrations']
allocation=json.loads((root/'root-allocation.json').read_text())
assert allocation['state']=='ROOT_ALLOCATED_ONCE' and allocation['batch']=='batch41-live-funded-v7'
assert allocation['expected_selection']==[['ark-testing::server',n] for n in proof['expected_names_in_execution_order']]
assert proof['source_peer_binding']['state']=='ACK_BOUND_TO_EXACT_FROZEN_SOURCE'
assert proof['profile']=='fallback-scale' and proof['per_test_timeout_seconds']==1740 and proof['per_lookup_poll_seconds']==120
assert proof['run_ignored']=='only' and proof['expected_ignored'] and proof['maximum_global_case_executions']==2
assert len(proof['expected_names_in_execution_order'])==2
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
