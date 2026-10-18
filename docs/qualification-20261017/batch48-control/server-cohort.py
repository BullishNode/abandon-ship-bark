import json, hashlib, sys, os, re, signal, subprocess, time, tarfile
from pathlib import Path
from datetime import datetime, timezone
root=Path('/fresh-cache/testing-track-20261017/batch48-sdk-watch-bridge-v11c-allocated')
proof=json.loads((root/'selection-proof.json').read_text())
assert os.environ.get('IN_NIX_SHELL'), 'source cached Nix before guard Python'
for key in ['BARK_DOUBLE_DRIVE_ACTIONS','LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'fully unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['expected_names_in_execution_order']==['lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds', 'exit::watchman_sweeps_funding_tx_during_exit', 'exit::watchman_sweeps_vtxo_chain_during_exit'] and proof['test_binaries_in_execution_order']==['ark-testing::bark-sdk']*3
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==3
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['threads']==1 and proof['retries']==0
assert proof['maximum_case_executions']==3 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is True and proof['expected_ignored'] is False and proof['run_ignored'] is None
raw=(root/'root-allocation.json').read_bytes();assert hashlib.sha256(raw).hexdigest()=='27007c4235840bc86630eff49041b000434bbdbc84dc60cbd832e694c201f379'==proof['root_authorization']['sha256']
allocation=json.loads(raw)
assert allocation['state']=='ALLOCATED_THREE_REPEAT_SDK_BRIDGE_CASES_AFTER_FRESH_GATES' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert allocation['binary']=='ark-testing::bark-sdk' and allocation['tests']==proof['expected_names_in_execution_order'] and allocation['runtime_container']=='abandon-fresh-runtime-20261017'
assert allocation['timeout_seconds']==180 and allocation['retries']==0 and allocation['serial']==1 and allocation['max_global_native']==2
assert allocation['requires_exact_CLN_and_both_plugins'] is True
for actual,key in [('source_manifest_sha256','source_manifest_sha256'),('archive_sha256','source_archive_sha256'),('bundle_manifest_sha256','bundle_manifest_sha256'),('immutable_metadata_sha256','immutable_metadata_sha256'),('runtime_metadata_sha256','runtime_metadata_sha256'),('runtime_pins_sha256','runtime_pinlist_sha256')]:
    assert allocation[actual]==proof[key]
for relative,expected in proof['proof_sha256'].items():assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected,relative
proposal=json.loads((root/'source-proposal.json').read_text());peer=json.loads((root/'source-peer.json').read_text())
assert allocation['source_proposal_sha256']==peer['proposal_sha256']==proof['proof_sha256']['source-proposal.json']
assert allocation['peer_sha256']==proof['proof_sha256']['source-peer.json']
bindings=json.loads((root/'source-bindings.json').read_text())
assert [item['name'] for item in bindings['functions']]==allocation['tests']
assert bindings['proposal_sha256']==allocation['source_proposal_sha256'] and bindings['peer_sha256']==allocation['peer_sha256']
assert len(bindings['full_files'])==12 and len(bindings['functions'])==3
prior=json.loads((root/'prior49-postflight.json').read_text())
assert prior['state']==proof['prior49_postflight']['required_state'] and prior['parent_terminal_confirmed']['exit_code']==0
assert not prior['cleanup_resources']['remaining_group_or_SID_members'] and not prior['cleanup_resources']['remaining_scoped_argv_cwd_FD_actors'] and not prior['cleanup_resources']['unreadable_owned_proc_entries']
assert hashlib.sha256(Path(proof['saved_listing']['path']).read_bytes()).hexdigest()==proof['saved_listing']['sha256']
saved=json.loads(Path(proof['saved_listing']['path']).read_text())
for suite in saved['rust-suites'].values():
    if suite['binary-id']=='ark-testing::bark-sdk':
        for name in allocation['tests']:assert name in suite['testcases'] and not suite['testcases'][name].get('ignored',False)
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==proof['runtime_pinlist_sha256']
pins={}
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    expected,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected,filename
    assert filename not in pins;pins[filename]=expected
assert len(pins)==proof['runtime_pin_count']==17
base='/fresh-cache/runtime-terminal-replay-v11c-20261018/'
for filename,key in [('source-manifest.json','source_manifest_sha256'),('manifest.json','bundle_manifest_sha256'),('native-bark-sdk','native_sdk_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:assert pins[base+filename]==proof[key]
for filename,key in [(proof['source_archive_path'],'source_archive_sha256'),(proof['immutable_metadata_path'],'immutable_metadata_sha256')]:
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==proof[key]
with tarfile.open(proof['source_archive_path']) as archive:
    source={item['path']:archive.extractfile(item['path']).read() for item in bindings['full_files']}
for item in bindings['full_files']:assert hashlib.sha256(source[item['path']]).hexdigest()==item['C_sha256']
for item in bindings['functions']+bindings['helpers']:
    file=item.get('file',item.get('source_file')); lines=source[file].decode().splitlines()
    fragment='\n'.join(lines[item['start_line']-1:item['end_line']]).lstrip(' \t')
    assert hashlib.sha256(fragment.encode()).hexdigest()==item['function_sha256'],item.get('function',item.get('name'))
    assert hashlib.sha256(fragment[fragment.index('{'):].encode()).hexdigest()==item['brace_body_sha256'],item.get('function',item.get('name'))
metadata=json.loads(Path(proof['metadata_path']).read_text())
assert set(metadata['rust-binaries'])=={'ark-testing::bark','ark-testing::bark-sdk','ark-testing::barkd','ark-testing::server'}
assert metadata['rust-binaries']['ark-testing::bark-sdk']['binary-path']==base+'native-bark-sdk'
for key,value in proof['execution_environment'].items():assert os.environ.get(key)==value,key
assert os.environ.get('LD_LIBRARY_PATH','').split(':')[0]==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('NEXTEST_PROFILE')=='default' and os.environ.get('CHAIN_SOURCE')=='bitcoind'
assert proof['native_CLN_required_version']=='v26.06.6'

def require_parent_launch_admission():
    assert not (root/'launch-authorization-pending.json').exists(), 'parent fresh gates pending'
    admission=json.loads((root/'parent-launch-admission.json').read_text());expected=[[binary,name] for binary,name in zip(proof['test_binaries_in_execution_order'],proof['expected_names_in_execution_order'])]
    assert admission['state']=='PARENT_HASH_GATE_PASSED' and admission['batch']=='batch48-sdk-watch-bridge-v11c-allocated'
    assert admission['source_manifest_sha256']==proof['source_manifest_sha256'] and admission['expected_selection']==expected
    assert admission['root_allocation_sha256']==proof['root_authorization']['sha256']
    required=['selection-proof.json', 'root-allocation.json', 'runtime-pins.sha256', 'source-proposal.json', 'source-peer.json', 'source-bindings.json', 'prior49-postflight.json', 'parent-actor-probe.py', 'server-cohort.py', 'preflight-static.py', 'fresh-listings.py', 'preflight.sh', 'run.sh', 'case01/nextest.toml', 'case02/nextest.toml', 'case03/nextest.toml', 'fresh-listing-admission.json', 'resource-admission.json', 'parent-actor-admission.json']
    assert set(admission['sha256'])==set(required)
    for rel in required:assert hashlib.sha256((root/rel).read_bytes()).hexdigest()==admission['sha256'][rel],rel
    listing=json.loads((root/'fresh-listing-admission.json').read_text());resource=json.loads((root/'resource-admission.json').read_text());actors=json.loads((root/'parent-actor-admission.json').read_text())
    assert listing['state']=='LISTING_GATE_PASS' and listing['selected_unignored']==expected
    assert resource['state']=='RESOURCE_GATE_PASS' and not resource['live_owned_postgres'] and resource['shm_available_bytes']>=12*1024*1024
    assert actors['state']=='PARENT_SCOPED_AND_CAPACITY_PASS' and actors['actor_count']==actors['unreadable_count']==0
    assert actors['global_native_count_before_launch']<=1 and actors['global_max_after_one_launch']==2 and actors['SHM_available_bytes']>=12*1024*1024
    assert admission['actor_probe_sha256']==proof['proof_sha256']['parent-actor-probe.py']
    assert admission['root_readonly_exemptions_reviewed'] is True, 'parent must independently classify any root-scoped exempted process'
require_parent_launch_admission()
assert not (root/'cohort-progress.json').exists() and not list(root.glob('case*/result.json')) and not list(root.glob('case*/test-data')), 'once only; preserve previous context/results'
if sys.argv[1:]==['--check-launch-authorization']:
    print('PARENT_LAUNCH_HASH_GATE_PASS');raise SystemExit(0)
assert not sys.argv[1:]

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
    'runtime_scope': 'Three repeated SDK expired-LN admission/refresh plus manual watchman payout names on capturedC; no newunique/currentV16/FULLML claim',
    'resource_cleanup': 'NONE; all raw evidence retained', 'retries': 0}, indent=2) + '\n')
