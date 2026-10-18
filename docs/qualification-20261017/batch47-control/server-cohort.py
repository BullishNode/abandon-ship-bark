import json, hashlib, sys, os, re, signal, subprocess, time
from pathlib import Path
from datetime import datetime, timezone

root = Path('/fresh-cache/testing-track-20261017/batch47-ordinary-recovery-three-v10')
proof = json.loads((root/'selection-proof.json').read_text())
assert os.environ.get('IN_NIX_SHELL'), 'cached Nix must be sourced before Python'
assert 'BARK_DOUBLE_DRIVE_ACTIONS' not in os.environ, 'presence enables double drive; fully unset after cached Nix'
for key in ['LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['state']=='ROOT_ALLOCATED_ONCE_PENDING_PARENT_LAUNCH'
assert proof['expected_names_in_execution_order']==['recovery::recovered_wallet_skips_exited_vtxo', 'recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit', 'recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub'] and proof['test_binaries_in_execution_order']==['ark-testing::barkd', 'ark-testing::barkd', 'ark-testing::barkd']
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==3
assert len(set(proof['expected_names_in_execution_order']))==3
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['retries']==0 and proof['threads']==1
assert proof['maximum_case_executions']==3 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is False and proof['expected_ignored'] is False and proof['run_ignored'] is None
allocation_bytes=(root/'root-allocation.json').read_bytes()
assert hashlib.sha256(allocation_bytes).hexdigest()=='96a9f25bd6e64925b4a028f8e5f0cdbf32a893edb891a9aee39ff434917782fb'==proof['root_authorization']['sha256']
allocation=json.loads(allocation_bytes)
assert allocation['state']=='ALLOCATED_FINALIZE_PEER_PREFLIGHT_THEN_ONE_PARENT_LAUNCH'
assert allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a' and allocation['cohort']=='batch47-ordinary-recovery-three-v10'
assert allocation['container']=='abandon-fresh-runtime-20261017'
assert allocation['binary']=='ark-testing::barkd' and allocation['cases']==['recovery::recovered_wallet_skips_exited_vtxo', 'recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit', 'recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub']
assert allocation['source_manifest_sha256']==proof['source_manifest_sha256'] and allocation['archive_sha256']==proof['source_archive_sha256']
assert allocation['metadata_sha256']==proof['runtime_metadata_sha256']
assert allocation['source_peer_sha256']==proof['source_peer_sha256'] and allocation['immutable_source_binding_sha256']==proof['immutable_source_binding_sha256']
assert allocation['timeout_seconds_each']==180 and allocation['retries']==0 and allocation['serial']==1
assert allocation['max_global_native']==2 and allocation['stop_on_first_failure'] is True and allocation['CLN_required'] is False and allocation['no_Cargo_shared_target_write'] is True
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==allocation['pins_sha256']==proof['runtime_pinlist_sha256']
for relative,expected_hash in [('source-peer.json',allocation['source_peer_sha256']),('immutable-source-proof.json',allocation['immutable_source_binding_sha256'])]:
    assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected_hash
peer=json.loads((root/'source-peer.json').read_text());binding=json.loads((root/'immutable-source-proof.json').read_text())
assert peer['proposal']['json_sha256']==binding['proposal']['sha256']=='9ae492d696dba9d010ca4873f1cc3b8720197d2dd6429f42801dd138d0087c0d'
assert [case['literal'] for case in peer['cases']]==['recovery::recovered_wallet_skips_exited_vtxo', 'recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit', 'recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub']
assert all(case['binary']=='ark-testing::barkd' and case['whole_file_sha256']=='97118c62993592f80fc167a1346168389c0386dae88a762cc99205ee417567b4' for case in peer['cases'])
actual_pins=[]
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    if not line.strip(): continue
    expected_hash,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:
        assert hashlib.file_digest(stream,'sha256').hexdigest()==expected_hash, filename
    actual_pins.append((expected_hash,filename))
assert len(actual_pins)==proof['runtime_pin_count']==17
with Path(proof['source_archive_path']).open('rb') as stream:
    assert hashlib.file_digest(stream,'sha256').hexdigest()==proof['source_archive_sha256']
assert hashlib.sha256(Path(proof['metadata_path']).read_bytes()).hexdigest()==proof['runtime_metadata_sha256']
assert hashlib.sha256(Path(proof['immutable_metadata_path']).read_bytes()).hexdigest()==proof['immutable_metadata_sha256']
pin_map={filename:sha for sha,filename in actual_pins}
base='/fresh-cache/runtime-sponsor-terminal-v10-20261018/'
for filename,key in [('manifest.json','bundle_manifest_sha256'),('source-manifest.json','source_manifest_sha256'),('native-barkd','native_barkd_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:
    assert pin_map[base+filename]==proof[key]
assert all(os.environ.get(key)==value for key,value in proof['execution_environment'].items() if not key.endswith('_VERSION') and key!='SQLITE_LIBRARY_PATH')
assert os.environ.get('SQLITE_LIBRARY_PATH')==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('LD_LIBRARY_PATH','').split(':')[0]==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('NEXTEST_PROFILE')=='default' and os.environ.get('CHAIN_SOURCE')=='bitcoind'

def require_parent_launch_admission():
    assert not (root/'launch-authorization-pending.json').exists(), 'parent admission pending'
    admission=json.loads((root/'parent-launch-admission.json').read_text())
    expected=[list(pair) for pair in zip(proof['test_binaries_in_execution_order'],proof['expected_names_in_execution_order'])]
    assert admission['state']=='PARENT_HASH_GATE_PASSED' and admission['batch']=='batch47-ordinary-recovery-three-v10'
    assert admission['expected_selection']==expected and admission['source_manifest_sha256']==proof['source_manifest_sha256']
    required=['selection-proof.json', 'root-allocation.json', 'runtime-pins.sha256', 'source-peer.json', 'immutable-source-proof.json', 'server-cohort.py', 'preflight-static.py', 'fresh-listings.py', 'preflight.sh', 'run.sh', 'case01/nextest.toml', 'case02/nextest.toml', 'case03/nextest.toml', 'fresh-listing-admission.json', 'resource-admission.json']
    assert set(admission['sha256'])==set(required)
    for relative in required:
        assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==admission['sha256'][relative],relative
    listing=json.loads((root/'fresh-listing-admission.json').read_text());resource=json.loads((root/'resource-admission.json').read_text())
    assert listing['state']=='LISTING_GATE_PASS' and listing['selected_unignored']==expected
    assert resource['state']=='RESOURCE_GATE_PASS' and not resource['live_owned_postgres'] and resource['shm_available_bytes']>=12*1024*1024

require_parent_launch_admission()
if sys.argv[1:]==['--check-launch-authorization']:
    print('PARENT_LAUNCH_HASH_GATE_PASS')
    raise SystemExit(0)
assert not sys.argv[1:], 'unknown runner argument'

results_path = root / 'cohort-progress.json'
assert not results_path.exists(), 'allocated case must run only once; preserve every prior outcome'
assert not list(root.glob('case*/result.json')), 'do not retry an allocated native case'
assert not list(root.glob('case*/test-data')), 'fresh contexts required'
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
    'runtime_scope': 'Three ordinary barkd recovery cases only after exact new root allocation; discovery-window or independently chain-confirmed Exited semantics only; no FULLML qualification',
    'resource_cleanup': 'NONE; all raw evidence retained', 'retries': 0}, indent=2) + '\n')
