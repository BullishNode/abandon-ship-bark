import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time
from datetime import datetime, timezone

root = Path('/fresh-cache/testing-track-20261017/batch22-upstream-bark-unstarted47')
proof = json.loads((root / 'selection-proof.json').read_text())
results_path = root / 'cohort-progress.json'
results = json.loads(results_path.read_text()) if results_path.exists() else []
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
        str(root / 'nextest-binaries-bark-runtime-immutable.json'),
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
    test_time = re.search(r'PASS \[\s*([0-9.]+)s\].*?ark-testing::bark ' + re.escape(name), text)
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
    'runtime_scope': 'Forty-seven previously unstarted upstream bark CLI tests; no automatic ML scenario qualification',
    'resource_cleanup': 'NONE; all raw evidence retained', 'retries': 0}, indent=2) + '\n')
