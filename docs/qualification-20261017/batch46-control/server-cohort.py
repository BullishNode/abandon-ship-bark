import json, hashlib, sys, os, re, signal, subprocess, time
from pathlib import Path
from datetime import datetime, timezone
root=Path('/fresh-cache/testing-track-20261017/batch46-lightning-physical-backup-v11c')
proof=json.loads((root/'selection-proof.json').read_text())
assert os.environ.get('IN_NIX_SHELL'), 'source cached Nix before guard Python'
for key in ['BARK_DOUBLE_DRIVE_ACTIONS','LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'fully unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['expected_names_in_execution_order']==['qualification_lightning_backup::qualification_internal_htlcs_ready_physical_backup_refunds_absent_sender'] and proof['test_binaries_in_execution_order']==['ark-testing::server']
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['threads']==1 and proof['retries']==0
assert proof['maximum_case_executions']==1 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is True and proof['native_CLN_required_version']=='v26.06.6'
assert proof['expected_ignored'] is False and proof['run_ignored'] is None
raw=(root/'root-allocation.json').read_bytes();assert hashlib.sha256(raw).hexdigest()=='9fff36edc955bc6159dea69d36ea27b1ccdd6bee830778c766c5c42fe363a5c2'==proof['root_authorization']['sha256']
allocation=json.loads(raw)
assert allocation['state']=='ALLOCATED_CONDITIONAL_ON_OWN47_FORMAL_TERMINAL_POSTFLIGHT' and allocation['owner_thread']=='01a14a4a-0a4d-7a62-8c6f-0041fdb9f53a'
assert allocation['binary']=='ark-testing::server' and allocation['test']=='qualification_lightning_backup::qualification_internal_htlcs_ready_physical_backup_refunds_absent_sender' and allocation['runtime_container']=='abandon-fresh-runtime-20261017'
assert allocation['source_file']==proof['source_file'] and allocation['source_file_sha256']==proof['source_file_sha256']=='6051ab33231cbe9cb88dad49fd8fa2baa852aade2bc519476c37ae24b17a870c'
for actual,key in [('source_manifest_sha256','source_manifest_sha256'),('archive_sha256','source_archive_sha256'),('bundle_manifest_sha256','bundle_manifest_sha256'),('immutable_metadata_sha256','immutable_metadata_sha256'),('runtime_metadata_sha256','runtime_metadata_sha256'),('runtime_pins_sha256','runtime_pinlist_sha256')]:
    assert allocation[actual]==proof[key]
assert allocation['timeout_seconds']==180 and allocation['retries']==0 and allocation['serial']==1 and allocation['max_global_native']==2 and allocation['requires_exact_CLN_and_both_plugins'] is True
assert len(allocation['conditions'])==7
for relative,expected in proof['proof_sha256'].items():
    assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected, relative
artifact=json.loads((root/'artifact-proof.json').read_text());source=json.loads((root/'source-proof.json').read_text());mechanical=json.loads((root/'mechanical-parent.json').read_text());peer=json.loads((root/'mechanical-peer.json').read_text())
assert artifact['P07']['source_sha256']==source['P07']['source_after6051']==mechanical['source_after_sha256']==peer['canonical_corrected_sha256']==proof['source_file_sha256']
assert artifact['P07']['async_body']['function_sha256']==mechanical['new_async_function_sha256']=='b475c2b79cac32137dbb41840fa98af2213dc74be4fe1e8a7bd4388d084ed60a'
assert artifact['P07']['async_body']['body_sha256']==mechanical['new_async_body_sha256']=='29508f4a0e30e1b329de4311207cd333fbcced0ca78e9f9a8ce981b50e55b85e'
assert len(artifact['P07']['all13_helpers'])==len(source['P07']['all13_helpers_identical_and_match_parent_ACK'])==13
for item in source['P07']['all13_helpers_identical_and_match_parent_ACK']:
    assert artifact['P07']['all13_helpers'][item['name']]=={key:item[key] for key in ['function_sha256','body_sha256']}
assert artifact['P07']['entry']['function_sha256']==source['P07']['entry_identical_and_parent_ACK_match']['function_sha256']=='aa91855bf97abe363fc77a10dbef2001c0fc79b4e0aca114f7d302c58a1a422b'
assert artifact['P07']['ExistingCluster_impl']['function_sha256']==source['P07']['ExistingCluster_impl_sha256']
assert artifact['P07']['module_source_sha256']==source['P07']['module_source_sha256'] and source['P07']['module_registered'] is True
assert isinstance(proof['required_47_postflight']['sha256'],str) and len(proof['required_47_postflight']['sha256'])==64, 'own47 formal receipt unknown; native prohibited'
assert proof['required_47_postflight']['sha256']==proof['proof_sha256']['batch47-postflight.json']
post=json.loads((root/'batch47-postflight.json').read_text())
assert post['state']=='TERMINAL_3PASS_POSTFLIGHT_SOURCE_ARTIFACT_CONTROL_INTEGRITY_PASS'
assert post['parent_terminal_confirmed']['tool_session']==28414 and post['parent_terminal_confirmed']['exit_code']==0
assert post['counts']['started']==post['counts']['PASS']==3 and post['counts']['FAIL']==post['counts']['NOT_STARTED']==0
assert post['all_source_control_result_and_log_hashes_before_after_unchanged'] is True
assert len(post['immutable']['all15parent_frozen_paths_unchanged'])==15 and len(post['cleanup_resources']['all17_actual_pins_unchanged'])==17
assert post['cleanup_resources']['remaining_group_or_SID_members']==[] and post['cleanup_resources']['remaining_scoped_argv_cwd_FD_actors']==[] and post['cleanup_resources']['unreadable_proc_entries']==[]
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==proof['runtime_pinlist_sha256']
pins={}
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    if not line.strip():continue
    expected,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected,filename
    assert filename not in pins;pins[filename]=expected
assert len(pins)==proof['runtime_pin_count']==17
base='/fresh-cache/runtime-terminal-replay-v11c-20261018/'
for filename,key in [('source-manifest.json','source_manifest_sha256'),('manifest.json','bundle_manifest_sha256'),('native-server','native_server_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:assert pins[base+filename]==proof[key]
for filename,key in [(proof['source_archive_path'],'source_archive_sha256'),(proof['immutable_metadata_path'],'immutable_metadata_sha256')]:
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==proof[key]
for key,value in proof['execution_environment'].items():assert os.environ.get(key)==value,key
assert os.environ.get('LD_LIBRARY_PATH','').split(':')[0]==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('NEXTEST_PROFILE')=='default' and os.environ.get('CHAIN_SOURCE')=='bitcoind'
cln=proof['execution_environment']['LIGHTNINGD_EXEC'];plugin=proof['execution_environment']['LIGHTNINGD_PLUGIN_DIR']
assert pins[cln]=='0979efd4a552759c51a6f05d774f85919556d942f926ae3682d1910374a4d2fc'
assert pins[plugin+'/cln-grpc']=='6a752d8fb498b5757e8e6efc68e64f04fe3732f61010fe2d55f2c9df9d9637d1' and pins[plugin+'/hold']=='cef52444d4ae4c7f819a315dc6deb4cfbb2b38d1156e17bf2bac271009e6c7e9'
pg=Path(proof['execution_environment']['POSTGRES_BINS'])/'pg_controldata'
with pg.open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==proof['pg_controldata_sha256']

def require_parent_launch_admission():
    assert not (root/'launch-authorization-pending.json').exists(), 'parent gates pending'
    admission=json.loads((root/'parent-launch-admission.json').read_text());expected=[['ark-testing::server','qualification_lightning_backup::qualification_internal_htlcs_ready_physical_backup_refunds_absent_sender']]
    assert admission['state']=='PARENT_HASH_GATE_PASSED' and admission['batch']=='batch46-lightning-physical-backup-v11c'
    assert admission['source_manifest_sha256']==proof['source_manifest_sha256'] and admission['expected_selection']==expected
    required=['selection-proof.json', 'root-allocation.json', 'runtime-pins.sha256', 'artifact-proof.json', 'source-proof.json', 'mechanical-parent.json', 'mechanical-peer.json', 'batch47-postflight.json', 'server-cohort.py', 'preflight-static.py', 'fresh-listings.py', 'preflight.sh', 'run.sh', 'case01/nextest.toml', 'fresh-listing-admission.json', 'resource-admission.json']
    assert set(admission['sha256'])==set(required)
    for rel in required:assert hashlib.sha256((root/rel).read_bytes()).hexdigest()==admission['sha256'][rel],rel
    listing=json.loads((root/'fresh-listing-admission.json').read_text());resource=json.loads((root/'resource-admission.json').read_text())
    assert listing['state']=='LISTING_GATE_PASS' and listing['selected_unignored']==expected
    assert resource['state']=='RESOURCE_GATE_PASS' and not resource['live_owned_postgres'] and resource['shm_available_bytes']>=12*1024*1024
require_parent_launch_admission()
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
    'runtime_scope': 'One clean internalHtlcsReady physical PG/server/watch backup; selected605k excludes continuingCLN12BTCreserve; no CLN-HSM/allboundary/FULLML qualification',
    'resource_cleanup': 'NONE; all raw evidence retained', 'retries': 0}, indent=2) + '\n')
