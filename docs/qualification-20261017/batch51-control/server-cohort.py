import json, hashlib, sys, os, re, signal, subprocess, time, tarfile
from pathlib import Path
from datetime import datetime, timezone
root=Path('/fresh-cache/testing-track-20261017/batch51-H32-H11-v19-v2')
proof=json.loads((root/'selection-proof.json').read_text())
assert hashlib.sha256(Path('/home/francis/abandon-ship-fresh-integration/justfile').read_bytes()).hexdigest()==proof['current_justfile_sha256'], 'current just recipe differs from captured V19'
assert os.environ.get('IN_NIX_SHELL'), 'source cached Nix before Python'
for key in ['BARK_DOUBLE_DRIVE_ACTIONS','LIGHTNINGD_DOCKER_IMAGE','TEST_POSTGRES_HOST','USE_FILESTORE','OLD_BARK_EXEC','OLD_CAPTAIND_EXEC']:
    assert key not in os.environ, 'fully unset presence override '+key
assert proof['candidate_ready'] is True and proof['controls_finalized'] is True and proof['execution_authorized'] is True
assert proof['expected_names_in_execution_order']==['qualification_dust_later_receipt::forfeited_small_receipt_does_not_revive_when_same_customer_receives_again', 'qualification_three_owner_forwarding::only_final_owner_receives_after_two_full_value_forwards'] and proof['test_binaries_in_execution_order']==['ark-testing::server']*2
assert len(proof['expected_names_in_execution_order'])==len(proof['test_binaries_in_execution_order'])==2
assert proof['profile']=='default' and proof['per_test_timeout_seconds']==180 and proof['threads']==1 and proof['retries']==0
assert proof['maximum_case_executions']==2 and proof['maximum_global_native']==2 and proof['first_fail_stops'] is True
assert proof['CLN_required'] is False and proof['expected_ignored'] is False and proof['run_ignored'] is None
assert hashlib.sha256((root/'root-preparation.json').read_bytes()).hexdigest()==proof['root_preparation_sha256']=='09dc9e68db34dee4e5e606c1a58a76a9775209a6cc108d714eaa779463c97b8d'
raw=(root/'root-allocation.json').read_bytes();assert hashlib.sha256(raw).hexdigest()==proof['root_authorization']['sha256']=='1df12af294e2871b863ea6a9f322ddfe9e1c63465e1c9d628d51722167ba7dca'
allocation=json.loads(raw)
assert allocation['state']=='EXECUTION_ALLOCATED_NOW' and allocation['cases']==proof['expected_names_in_execution_order']
assert allocation['container']=='abandon-fresh-runtime-20261017' and allocation['cwd']=='/home/francis/abandon-ship-fresh-integration'
budget=allocation['budget'];assert budget=={'per_case_seconds':180,'retries':0,'serial':1,'max_global_native':2,'cases':2}
legacy=allocation['root_legacy_result'];assert legacy['state']=='PASS' and legacy['own_scoped_actors']==legacy['unreadable_proc_entries']==0
assert legacy['formal_postflight_sha256']==proof['root_legacy_postflight_sha256']
assert hashlib.sha256(Path(proof['root_legacy_postflight_path']).read_bytes()).hexdigest()==proof['root_legacy_postflight_sha256']
for actual,key in [('source_manifest_sha256','source_manifest_sha256'),('bundle_manifest_sha256','bundle_manifest_sha256'),('metadata_immutable_sha256','immutable_metadata_sha256'),('metadata_runtime_sha256','runtime_metadata_sha256'),('runtime_pins_sha256','runtime_pinlist_sha256')]:assert allocation[actual]==proof[key]
assert allocation['metadata']==proof['metadata_path']
for relative,expected in proof['proof_sha256'].items():assert hashlib.sha256((root/relative).read_bytes()).hexdigest()==expected,relative
assert hashlib.sha256((root/'runtime-pins.sha256').read_bytes()).hexdigest()==proof['runtime_pinlist_sha256']
pins={}
for line in (root/'runtime-pins.sha256').read_text().splitlines():
    expected,filename=line.split(None,1);filename=filename.strip()
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected,filename
    assert filename not in pins;pins[filename]=expected
assert len(pins)==proof['runtime_pin_count']==16
base='/fresh-cache/runtime-extra-money-fixtures-v19-20261018/'
for filename,key in [('source-manifest.json','source_manifest_sha256'),('manifest.json','bundle_manifest_sha256'),('native-server','native_server_sha256'),('nextest-binaries-runtime.json','runtime_metadata_sha256')]:assert pins[base+filename]==proof[key]
manifest=json.loads(Path(base+'manifest.json').read_text())
for product,expected in proof['products'].items():
    assert manifest['products'][product]==expected
    executable=os.environ.get(product.upper()+'_EXEC');assert executable==expected['path']
    assert pins[executable]==expected['sha256']
    with Path(executable).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==expected['sha256']
for filename,key in [(proof['source_archive_path'],'source_archive_sha256'),(proof['immutable_metadata_path'],'immutable_metadata_sha256')]:
    with Path(filename).open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==proof[key]
sourceproof=json.loads((root/'source-artifact-proof.json').read_text());assert sourceproof['source_manifest_sha256']==proof['source_manifest_sha256'] and sourceproof['source_archive_sha256']==proof['source_archive_sha256']
assert [case['selector'] for case in sourceproof['our_source_bindings']]==proof['expected_names_in_execution_order']
filepins={path:record['captured_sha256'] for path,record in sourceproof['whole_file_API_schema_helper_bindings'].items()}
for case in sourceproof['our_source_bindings']:filepins[case['file']]=case['full_file_sha256']
with tarfile.open(proof['source_archive_path']) as archive:source={path:archive.extractfile(path).read() for path in filepins}
for path,expected in filepins.items():assert hashlib.sha256(source[path]).hexdigest()==expected,path
for case in sourceproof['our_source_bindings']:
    assert case['explicit_module'] in source['testing/tests/server/mod.rs'].decode()
    for item in [case['entry_function']]+case['local_helpers_and_implementation_regions']:
        lines=source[item['file']].decode().splitlines();fragment='\n'.join(lines[item['start_line']-1:item['end_line']]).lstrip(' \t')
        assert hashlib.sha256(fragment.encode()).hexdigest()==item['function_sha256']
        assert hashlib.sha256(fragment[fragment.index('{'):].encode()).hexdigest()==item['brace_body_sha256']
metadata=json.loads(Path(proof['metadata_path']).read_text());immutable=json.loads(Path(proof['immutable_metadata_path']).read_text())
assert set(metadata['rust-binaries'])=={'ark-testing::server'}
assert metadata['rust-binaries']==immutable['rust-binaries'] and metadata['rust-binaries']['ark-testing::server']['binary-path']==base+'native-server'
a=immutable['rust-build-meta']['non-test-binaries'];b=metadata['rust-build-meta']['non-test-binaries'];assert a.keys()==b.keys();changes=0
for package,entries in a.items():
    assert len(entries)==len(b[package])
    for old,new in zip(entries,b[package]):
        assert {k:v for k,v in old.items() if k!='path'}=={k:v for k,v in new.items() if k!='path'}
        assert new['path']=='../runtime-extra-money-fixtures-v19-20261018/support/'+old['path']
        changes+=old['path']!=new['path']
assert changes==3
for key,value in proof['execution_environment'].items():assert os.environ.get(key)==value,key
assert os.environ.get('LD_LIBRARY_PATH','').split(':')[0]==proof['execution_environment']['SQLITE_LIBRARY_PATH']
assert os.environ.get('NEXTEST_PROFILE')=='default' and os.environ.get('CHAIN_SOURCE')=='bitcoind'

def require_parent_launch_admission():
    assert not (root/'launch-authorization-pending.json').exists(), 'parent fresh gates pending'
    admission=json.loads((root/'parent-launch-admission.json').read_text());expected=[['ark-testing::server',name] for name in proof['expected_names_in_execution_order']]
    assert admission['state']=='PARENT_HASH_GATE_PASSED' and admission['batch']=='batch51-H32-H11-v19-v2'
    assert admission['source_manifest_sha256']==proof['source_manifest_sha256'] and admission['expected_selection']==expected
    assert admission['root_allocation_sha256']==proof['root_authorization']['sha256']
    required=['selection-proof.json', 'root-preparation.json', 'root-allocation.json', 'runtime-pins.sha256', 'source-artifact-proof.json', 'runtime-overlay-proof.json', 'parent-actor-probe.py', 'server-cohort.py', 'preflight-static.py', 'fresh-listings.py', 'preflight.sh', 'run.sh', 'case01/nextest.toml', 'case02/nextest.toml', 'fresh-listing-admission.json', 'resource-admission.json', 'parent-actor-admission.json']
    assert set(admission['sha256'])==set(required)
    for rel in required:assert hashlib.sha256((root/rel).read_bytes()).hexdigest()==admission['sha256'][rel],rel
    listing=json.loads((root/'fresh-listing-admission.json').read_text());resource=json.loads((root/'resource-admission.json').read_text());actors=json.loads((root/'parent-actor-admission.json').read_text())
    assert listing['state']=='LISTING_GATE_PASS' and listing['selected_unignored']==expected
    assert resource['state']=='RESOURCE_GATE_PASS' and not resource['live_owned_postgres'] and resource['shm_available_bytes']>=256*1024*1024
    assert actors['protected_compiler_count']==0
    assert actors['state']=='PARENT_SCOPED_AND_CAPACITY_PASS' and actors['actor_count']==actors['unreadable_count']==0
    assert actors['global_native_count_before_launch']<=1 and actors['global_max_after_one_launch']==2 and actors['SHM_available_bytes']>=256*1024*1024
    assert actors['disk_available_bytes']>=20*1024**3 and resource['disk_available_bytes']>=20*1024**3
    assert admission['root_readonly_exemptions_reviewed'] is True
    assert admission['actor_probe_sha256']==proof['proof_sha256']['parent-actor-probe.py']
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
        'live_owned_postgres': live_owned_pg, 'disk_available_bytes': os.statvfs('/fresh-cache').f_bavail*os.statvfs('/fresh-cache').f_frsize, 'cleanup_performed': False}
    (directory / 'resource-preflight.json').write_text(json.dumps(record, indent=2) + '\n')
    if available < 256 * 1024 * 1024 or record['disk_available_bytes']<20*1024**3 or live_owned_pg:
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
        'retries': 0, 'no_new_compilation': True, 'expected_binary':'ark-testing::server'}
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
            selection = re.search(r'Starting (\d+) tests? across (\d+) binar', text)
            if match or (selection and selection.groups() != ('1','1')):
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
    selected = re.search(r'Starting (\d+) tests? across (\d+) binar', text)
    run_id = re.search(r'Nextest run ID ([a-f0-9-]+)', text)
    test_time = re.search(r'PASS \[\s*([0-9.]+)s\].*?' + re.escape(proof['test_binaries_in_execution_order'][index - 1]) + ' ' + re.escape(name), text)
    fresh=json.loads((directory/'selection-list-fresh.json').read_text())
    filtered=sum(1 for suite in fresh['rust-suites'].values() for test in suite['testcases'].values() if test['filter-match']['status']!='matches')
    summary=re.search(r'Summary.*?1 test run: 1 passed(?: \(1 slow\))?, (\d+) skipped',text)
    good = code == 0 and selected and selected.groups()==('1','1') and test_time and summary and int(summary.group(1))==filtered and not resource_error
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
    'runtime_scope': 'Two explicitly allocated H32/H11 native fixtures on capturedV19 tests/inheritedV18 products; case-specific assertions only, no fullML/globalproduction qualification',
    'resource_cleanup': 'NONE; all raw evidence retained', 'retries': 0}, indent=2) + '\n')
