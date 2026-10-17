import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone

root = Path('/fresh-cache/runtime-owner-scan-split-v1-20261017')
repo = Path('/home/francis/abandon-ship-fresh-integration')
source = json.loads((root / 'source-manifest.json').read_text())
def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
def write(name, value):
    (root / name).write_text(json.dumps(value, indent=2) + '\n')
def verify(stage):
    assert subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip() == source['head']
    assert not [n for n, h in source['files'].items() if sha(repo / n) != h]
    assert hashlib.sha256(subprocess.check_output(['git', 'diff', '--binary'], cwd=repo)).hexdigest() == source['actual_git_diff_sha256']
    write('source-verification-extra-' + stage + '.json', {'state': 'PASS', 'files': len(source['files']),
        'at': datetime.now(timezone.utc).isoformat()})

verify('before')
command = ['cargo', 'nextest', 'list', '--package', 'ark-testing', '--test', 'bark', '--test', 'barkd',
    '--test', 'core', '--features', 'ark-testing/fallback-test-bip84', '--list-type', 'binaries-only',
    '--message-format', 'json']
receipt = {'state': 'RUNNING', 'command': command, 'started_at': datetime.now(timezone.utc).isoformat(),
    'allocation': 'Coordinator approved extra immutable metadata only after sole oversize case started; retain sole Cargo lease'}
write('extra-metadata-receipt.json', receipt)
print('extra metadata: START', flush=True)
started = time.monotonic()
with (root / 'nextest-binaries-extra.json').open('wb') as output, (root / 'extra-metadata.log').open('wb') as log:
    proc = subprocess.run(command, cwd=repo, stdout=output, stderr=log)
receipt.update(state='PASS' if proc.returncode == 0 else 'FAIL', exit_code=proc.returncode,
    seconds=round(time.monotonic() - started, 3), ended_at=datetime.now(timezone.utc).isoformat())
write('extra-metadata-receipt.json', receipt)
if proc.returncode:
    sys.exit(proc.returncode)
verify('after')
extra = json.loads((root / 'nextest-binaries-extra.json').read_text())
assert set(extra['rust-binaries']) == {'ark-testing::bark', 'ark-testing::barkd', 'ark-testing::core'}
products = {}
for name, item in extra['rust-binaries'].items():
    target = root / ('native-' + item['binary-name'])
    shutil.copy2(item['binary-path'], target)
    item['binary-path'] = str(target)
    products[name] = {'path': str(target), 'sha256': sha(target)}
write('nextest-binaries-extra-immutable.json', extra)
paired = json.loads((root / 'nextest-binaries-immutable.json').read_text())
assert extra['rust-build-meta'] == paired['rust-build-meta'], 'build-meta differs; reconcile before combining'
paired['rust-binaries'].update(extra['rust-binaries'])
write('nextest-binaries-default-five-immutable.json', paired)
list_command = ['cargo', 'nextest', 'list', '--binaries-metadata', str(root / 'nextest-binaries-default-five-immutable.json'),
    '--message-format', 'json']
with (root / 'default-five-test-list.json').open('wb') as output, (root / 'default-five-test-list.log').open('wb') as log:
    subprocess.run(list_command, cwd=repo, stdout=output, stderr=log, check=True)
verify('final')
manifest = {'head': source['head'], 'dirty': True, 'source_manifest_sha256': sha(root / 'source-manifest.json'),
    'paired_candidate_manifest_sha256': sha(root / 'manifest.json'), 'products': products,
    'extra_metadata_sha256': sha(root / 'nextest-binaries-extra-immutable.json'),
    'default_five_metadata_sha256': sha(root / 'nextest-binaries-default-five-immutable.json'),
    'default_five_test_list_sha256': sha(root / 'default-five-test-list.json'),
    'extra_metadata_build': receipt, 'native_execution': 'NONE; inventory only, all default integration cases NOT RUN by this metadata step',
    'scope': 'bark,barkd,core plus identical earlier SDK/server; migrations/upgrades/tor separate gates',
    'captured_at': datetime.now(timezone.utc).isoformat()}
write('default-five-inventory-manifest.json', manifest)
print('extra metadata: IMMUTABLE CAPTURE COMPLETE ' + str(receipt['seconds']) + 's', flush=True)
