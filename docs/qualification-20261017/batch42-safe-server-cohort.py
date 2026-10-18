import json
import hashlib
import sys
import os
from pathlib import Path
import re
import signal
import subprocess
import time
from datetime import datetime, timezone

root = Path('/fresh-cache/testing-track-20261017/batch42-ordinary-twelve-v8')
proof = json.loads((root / 'selection-proof.json').read_text())

assert proof.get('candidate_ready') is True, 'exact v8 candidate pins pending; no listing/preflight/native authorized'
assert proof.get('execution_authorized') is True and proof['state']=='ROOT_ALLOCATED_ONCE_PENDING_PARENT_LAUNCH', 'explicit exact v8 root allocation pending'
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==12
assert len(set(proof['expected_names_in_execution_order']))==12
assert proof['expected_names_in_execution_order']==['onchain::list_utxos', 'onchain::onchain_drain', 'movement::board_board', 'movement::movement_offboard', 'movement::movement_send_onchain', 'offboard::bark_send_onchain', 'offboard::bark_rejects_offboarding_dust_amount', 'boards::board_all_barkd', 'recovery::recovered_wallet_finds_boarded_vtxo', 'recovery::recovered_wallet_finds_mixed_origin_vtxos', 'recovery::recovered_wallet_finds_round_vtxo', 'recovery::recovered_wallet_quarantines_invalid_spent_vtxos']
assert proof['test_binaries_in_execution_order']==['ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::bark', 'ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd']
assert proof['test_binaries_in_execution_order'].count('ark-testing::bark')==7 and proof['test_binaries_in_execution_order'].count('ark-testing::barkd')==5
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['maximum_case_executions']==12
assert not proof['expected_ignored'] and proof['run_ignored'] is None
for key in ['source_manifest_sha256','bundle_manifest_sha256','runtime_metadata_sha256']:
    value=proof[key]
    assert isinstance(value,str) and len(value)==64 and all(c in '0123456789abcdef' for c in value), 'candidate hash pending'
assert isinstance(proof['metadata_path'],str) and proof['metadata_path'].startswith('/fresh-cache/') and 'PENDING' not in proof['metadata_path']


def require_parent_launch_admission():
    assert not (root / 'launch-authorization-pending.json').exists(), 'parent launch/hash gate pending; no native case authorized from this runner yet'
    allocation = json.loads((root / 'root-allocation.json').read_text())
    admission = json.loads((root / 'parent-launch-admission.json').read_text())
    expected = [list(pair) for pair in zip(proof['test_binaries_in_execution_order'],proof['expected_names_in_execution_order'])]
    assert allocation['state'] == 'AUTHORIZED_EXTERNAL_ORDINARY12_SERIAL_LANE2_AFTER_OWN_PREFLIGHT' and [[row['binary'],row['name']] for row in allocation['exact_cases']] == expected
    assert allocation['source_manifest_sha256']==proof['source_manifest_sha256'] and allocation['bundle_manifest_sha256']==proof['bundle_manifest_sha256']
    assert admission['state'] == 'PARENT_HASH_GATE_PASSED' and admission['batch'] == 'batch42-ordinary-twelve-v8'
    assert admission['expected_selection'] == expected and admission['source_manifest_sha256'] == proof['source_manifest_sha256']
    required = ['selection-proof.json', 'root-allocation.json', 'runtime-pins.sha256', 'server-cohort.py', 'preflight-static.py', 'fresh-listings.py', 'preflight.sh', 'run.sh', 'case01/nextest.toml', 'case02/nextest.toml', 'case03/nextest.toml', 'case04/nextest.toml', 'case05/nextest.toml', 'case06/nextest.toml', 'case07/nextest.toml', 'case08/nextest.toml', 'case09/nextest.toml', 'case10/nextest.toml', 'case11/nextest.toml', 'case12/nextest.toml', 'fresh-listing-admission.json', 'resource-admission.json']
    assert set(admission['sha256']) == set(required), 'parent freeze must cover every control and fresh admission'
    for relative in required:
        assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == admission['sha256'][relative], relative
    listing = json.loads((root / 'fresh-listing-admission.json').read_text())
    resource = json.loads((root / 'resource-admission.json').read_text())
    assert listing['state'] == 'LISTING_GATE_PASS' and listing['selected_unignored'] == expected
    assert resource['state'] == 'RESOURCE_GATE_PASS' and not resource['live_owned_postgres']
    assert resource['shm_available_bytes'] >= 12 * 1024 * 1024

require_parent_launch_admission()
if sys.argv[1:] == ['--check-launch-authorization']:
    print('PARENT_LAUNCH_HASH_GATE_PASS')
    raise SystemExit(0)
assert not sys.argv[1:], 'unknown runner argument'

results_path = root / 'cohort-progress.json'
assert not results_path.exists(), 'allocated case must run only once; preserve every prior outcome'
assert not list(root.glob('case*/result.json')), 'do not retry an allocated native case'
results = []
assert all(x['state'] == 'PASS' for x in results), 'prior case requires reconciliation; do not retry'
completed_names = [x['name'] for x in results]
assert completed_names == proof['expected_names_in_execution_order'][:len(results)]
resource_pattern = re.compile(r'No space left on device|could not resize shared memory|Cannot allocate memory|'
    r'Resource temporarily unavailable|Too many open files|could not fork|'
    r'failed to spawn[^\n]*os error (?:11|12|24|28)', re.IGNORECASE)

def save():
    (root / 'cohort-progress.json').write_text(json.dumps(results, indent=2) + '\n')

def preflight(directory):
    stat = os.statvfs('/dev/shm')
    available = stat.f_bavail * stat.f_frsize
    live_owned_pg = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            comm = (entry / 'comm').read_text().strip()
            args = (entry / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            continue
        if comm == 'postgres' and '/fresh-cache/testing-track-20261017/' in args:
            live_owned_pg.append(int(entry.name))
    record = {'at': datetime.now(timezone.utc).isoformat(), 'shm_available_bytes': available,
        'live_owned_postgres': live_owned_pg, 'cleanup_performed': False}
    (directory / 'resource-preflight.json').write_text(json.dumps(record, indent=2) + '\n')
    if available < 12 * 1024 * 1024 or live_owned_pg:
        raise RuntimeError('resource preflight blocked; preserve state and request coordinator cleanup')

for index, name in enumerate(proof['expected_names_in_execution_order'], 1):
    directory = root / ('case' + str(index).zfill(2))
    if name in completed_names:
        continue
    try:
        preflight(directory)
    except RuntimeError as error:
        event = {'state': 'BLOCKED_RESOURCE_PREFLIGHT', 'case_index': index, 'name': name, 'test_started': False, 'retry': False, 'at': datetime.now(timezone.utc).isoformat(), 'reason': str(error)}
        (directory / ('resource-stop-' + str(time.time_ns()) + '.json')).write_text(json.dumps(event, indent=2) + '\n')
        print('BLOCKED before case ' + str(index) + ': ' + str(error), flush=True)
        raise SystemExit(75)
    env = os.environ.copy()
    env['TEST_DIRECTORY'] = str(directory / 'test-data')
    command = ['just', '--set', 'JUSTFILE_DIR', str(root), 'int-binaries',
        proof['metadata_path'],
        '--test-threads 1 --retries 0 --config-file ' + str(directory / 'nextest.toml') +
        ' -- ' + name + ' --exact']
    record = {'index': index, 'name': name, 'state': 'RUNNING', 'command': command,
        'started_at': datetime.now(timezone.utc).isoformat(), 'per_test_timeout_seconds': 180,
        'retries': 0, 'no_new_compilation': True}
    results.append(record)
    save()
    started = time.monotonic()
    print('START ' + name, flush=True)
    with (directory / 'nextest.log').open('wb') as log:
        child = subprocess.Popen(command, cwd='/home/francis/abandon-ship-fresh-integration',
            env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        record['owned_nextest_group'] = child.pid
        save()
        resource_error = None
        while child.poll() is None:
            text = (directory / 'nextest.log').read_text(errors='replace')
            match = resource_pattern.search(text)
            selection = re.search(r'Starting (\d+) tests? across', text)
            if match or (selection and selection.group(1) != '1'):
                resource_error = match.group(0) if match else 'Unexpected test selection count'
                os.killpg(child.pid, signal.SIGINT)
                try:
                    child.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGTERM)
                    child.wait(timeout=20)
                break
            time.sleep(0.5)
        code = child.wait()
    text = (directory / 'nextest.log').read_text(errors='replace')
    selected = re.search(r'Starting (\d+) tests? across', text)
    run_id = re.search(r'Nextest run ID ([a-f0-9-]+)', text)
    test_time = re.search(r'PASS \[\s*([0-9.]+)s\].*?' + re.escape(proof['test_binaries_in_execution_order'][index - 1]) + ' ' + re.escape(name), text)
    good = code == 0 and selected and selected.group(1) == '1' and test_time and not resource_error
    record.update(state='PASS' if good else 'FAIL', exit_code=code,
        resource_error=resource_error, run_id=run_id.group(1) if run_id else None,
        test_seconds=float(test_time.group(1)) if test_time else None,
        wall_seconds=round(time.monotonic() - started, 3), ended_at=datetime.now(timezone.utc).isoformat())
    (directory / 'result.json').write_text(json.dumps(record, indent=2) + '\n')
    save()
    print(record['state'] + ' ' + name, flush=True)
    if index % 4 == 0 or index == len(proof['expected_names_in_execution_order']):
        stat = os.statvfs('/dev/shm')
        checkpoints = root / 'resource-checkpoints.jsonl'
        with checkpoints.open('a') as checkpoint:
            checkpoint.write(json.dumps({'completed': index, 'available_shm_bytes': stat.f_bavail * stat.f_frsize, 'at': datetime.now(timezone.utc).isoformat(), 'cleanup': False}) + '\n')
    if not good:
        raise SystemExit(code if code else 101)

(root / 'cohort-summary.json').write_text(json.dumps({
    'result': 'PASS', 'passed': len(results), 'failed': 0, 'cases': results,
    'source_manifest_sha256': proof['source_manifest_sha256'], 'metadata_sha256': proof['runtime_metadata_sha256'],
    'runtime_scope': 'Twelve explicitlyallocatedordinary v8 adapters7CLI/5barkd; no fullsuite/fullML qualification',
    'resource_cleanup': 'NONE; all raw evidence retained', 'retries': 0}, indent=2) + '\n')
