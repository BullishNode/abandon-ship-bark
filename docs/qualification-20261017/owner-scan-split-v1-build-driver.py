import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone

root = Path('/fresh-cache/runtime-owner-scan-split-v1-20261017')
repo = Path('/home/francis/abandon-ship-fresh-integration')
source = json.loads((root / 'source-manifest.json').read_text())
results = {}

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def write(name, data):
    (root / name).write_text(json.dumps(data, indent=2) + '\n')

def verify(stage):
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()
    changed = [name for name, expected in source['files'].items() if sha(repo / name) != expected]
    patch = subprocess.check_output(['git', 'diff', '--binary'], cwd=repo)
    assert head == source['head'] and not changed, (stage, head, changed)
    assert hashlib.sha256(patch).hexdigest() == source['actual_git_diff_sha256'], stage
    write('source-verification-' + stage + '.json', {'state': 'PASS', 'head': head,
        'files': len(source['files']), 'at': datetime.now(timezone.utc).isoformat()})

def phase(name, command, stdout_name=None):
    verify('before-' + name)
    started = time.monotonic()
    record = {'state': 'RUNNING', 'command': command,
        'started_at': datetime.now(timezone.utc).isoformat()}
    results[name] = record
    write('build-phases.json', results)
    print(name + ': START', flush=True)
    with (root / (name + '.log')).open('wb') as log:
        if stdout_name:
            with (root / stdout_name).open('wb') as output:
                proc = subprocess.run(command, cwd=repo, stdout=output, stderr=log)
        else:
            proc = subprocess.run(command, cwd=repo, stdout=log, stderr=subprocess.STDOUT)
    record.update(state='PASS' if proc.returncode == 0 else 'FAIL', exit_code=proc.returncode,
        seconds=round(time.monotonic() - started, 3), ended_at=datetime.now(timezone.utc).isoformat())
    write('build-phases.json', results)
    print(name + ': ' + record['state'] + ' ' + str(record['seconds']) + 's', flush=True)
    if proc.returncode:
        sys.exit(proc.returncode)
    verify('after-' + name)

phase('checks', ['just', 'checks'])
phase('units', ['just', '--set', 'JUSTFILE_DIR', str(root), 'unit', '--test-threads 4'])
phase('products', ['cargo', 'build', '--package', 'bark-server', '--bin', 'captaind', '--bin', 'watchmand',
    '--package', 'bark-cli', '--bin', 'bark', '--bin', 'barkd',
    '--features', 'bark-cli/fallback-test-bip84,bark-cli/barkd-web-ui'])
products = {}
for name in ['captaind', 'watchmand', 'bark', 'barkd']:
    path = root / name
    shutil.copy2(Path('/fresh-cache/target/debug') / name, path)
    products[name] = {'path': str(path), 'sha256': sha(path)}
    if name in ['bark', 'barkd']:
        products[name].update(features='bark-cli/fallback-test-bip84,bark-cli/barkd-web-ui',
            web_dist='/fresh-cache/web-dist-10ecfb9')
write('products-capture.json', products)
print('products: IMMUTABLE CAPTURE COMPLETE', flush=True)
phase('metadata', ['cargo', 'nextest', 'list', '--package', 'ark-testing', '--test', 'bark-sdk',
    '--test', 'server', '--features', 'ark-testing/fallback-test-bip84', '--list-type', 'binaries-only',
    '--message-format', 'json'], stdout_name='nextest-binaries.json')
metadata = json.loads((root / 'nextest-binaries.json').read_text())
assert set(metadata['rust-binaries']) == {'ark-testing::server', 'ark-testing::bark-sdk'}
for binary_id, item in metadata['rust-binaries'].items():
    target = root / ('native-' + item['binary-name'])
    shutil.copy2(item['binary-path'], target)
    item['binary-path'] = str(target)
    products[binary_id] = {'path': str(target), 'sha256': sha(target)}
write('nextest-binaries-immutable.json', metadata)
support = {}
for entries in metadata['rust-build-meta'].get('non-test-binaries', {}).values():
    for item in entries:
        original = Path('/fresh-cache/target') / item['path']
        target = root / 'support' / item['path']
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(original, target)
        support[item['path']] = {'path': str(target), 'sha256': sha(target)}
write('support-capture.json', support)
verify('final-capture')
manifest = {'head': source['head'], 'dirty': True,
    'source_manifest_sha256': sha(root / 'source-manifest.json'),
    'source_archive_sha256': sha(root / 'source.tar.gz'),
    'source_scope': source['source_scope'], 'actual_git_diff_sha256': source['actual_git_diff_sha256'],
    'products': products, 'metadata_path': str(root / 'nextest-binaries-immutable.json'),
    'metadata_sha256': sha(root / 'nextest-binaries-immutable.json'),
    'web_dist_manifest_sha256': sha(root / 'web-dist-manifest.json'), 'build_phases': results,
    'support_capture_sha256': sha(root / 'support-capture.json'),
    'source_verification': 'All1156trackedfiles,HEADanddirtydiff unchanged before/after every phase and final capture',
    'native_result': 'NOT RUN; exactly one corrected oversized case allocated after capture',
    'nix': 'Existing cached sourced environment; no Nix derivation built; exact actual CLN adapter separately pinned where used',
    'captured_at': datetime.now(timezone.utc).isoformat()}
write('manifest.json', manifest)
print('ALL IMMUTABLE CAPTURE COMPLETE source=' + manifest['source_manifest_sha256'] +
    ' metadata=' + manifest['metadata_sha256'] + ' manifest=' + sha(root / 'manifest.json'), flush=True)
