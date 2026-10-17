import json, os
from pathlib import Path
root=Path('/fresh-cache/testing-track-20261017/next-upstream-barkd-unstarted36')
proof=json.loads((root/'selection-proof.json').read_text())
assert len(proof['expected_names_in_execution_order'])==36
assert not list(root.glob('case*/result.json')), 'batch has started; do not treat preparation as fresh'
stat=os.statvfs('/dev/shm'); available=stat.f_bavail*stat.f_frsize
owned=[]
for p in Path('/proc').iterdir():
 if not p.name.isdigit(): continue
 try:
  if (p/'comm').read_text().strip()=='postgres' and '/fresh-cache/testing-track-20261017/' in (p/'cmdline').read_bytes().replace(b'\0',b' ').decode(errors='replace'): owned.append(int(p.name))
 except (FileNotFoundError,PermissionError,ProcessLookupError): pass
assert available>=12*1024*1024 and not owned, 'resource gate blocked; preserve evidence; request root cleanup'
(root/'resource-admission.json').write_text(json.dumps({'state':'RESOURCE_GATE_PASS','shm_available_bytes':available,'live_owned_postgres':owned,'cleanup':False})+'\n')
