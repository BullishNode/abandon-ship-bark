import json, os, re
from pathlib import Path
from datetime import datetime, timezone

assert os.environ.get('IN_NIX_SHELL')
assert 'BARK_DOUBLE_DRIVE_ACTIONS' not in os.environ
root = Path('/fresh-cache/testing-track-20261017/batch50-H30-ML125-v17')
labels = ['batch45-recovery-three-v10', 'batch47-ordinary-recovery-three-v10', 'batch46-lightning-physical-backup-v11c', 'batch49-default-daemon-singleton-v11c', 'batch48-sdk-watch-bridge-v11c-allocated', 'batch50-H30-ML125-v17']
scopes = [prefix + label for prefix in ['/fresh-cache/testing-track-20261017/', '/home/francis/.cache/abandon-fresh/testing-track-20261017/'] for label in labels]
scopes += ['/fresh-cache/independent-two-v11c-20261018', '/home/francis/.cache/abandon-fresh/independent-two-v11c-20261018']
known_groups = {143297, 143753, 144233, 145878, 146890, 148754, 154295, 165688, 173577, 174944, 175188}
root_scopes = ['/fresh-cache/root-claim-backfill-v16-20261018', '/home/francis/.cache/abandon-fresh/root-claim-backfill-v16-20261018', '/fresh-cache/root-reserved-partial-v17-20261018', '/home/francis/.cache/abandon-fresh/root-reserved-partial-v17-20261018']
root_readonly_exempted = []
excluded, current = set(), os.getpid()
while current and current not in excluded:
    excluded.add(current)
    try:
        current = int((Path('/proc') / str(current) / 'stat').read_text().rsplit(')', 1)[1].split()[1])
    except (OSError, ValueError, IndexError):
        break
actors, unreadable, native = [], [], []
for process in Path('/proc').iterdir():
    if not process.name.isdigit() or int(process.name) in excluded:
        continue
    try:
        status = (process / 'status').read_text()
        uid = int(re.search(r'^Uid:\s+(\d+)', status, re.M).group(1))
        if uid != os.getuid():
            continue
        stat = (process / 'stat').read_text().rsplit(')', 1)[1].split()
        if stat[0] == 'Z':
            continue
        argv = (process / 'cmdline').read_bytes().split(b'\0')
        command = b' '.join(argv).decode(errors='replace')
        reasons = []
        if any(scope in command for scope in scopes):
            reasons.append('argv_scope')
        if int(stat[2]) in known_groups or int(stat[3]) in known_groups:
            reasons.append('known_previous_group_or_SID')
        if any(scope in os.readlink(process / 'cwd') for scope in scopes):
            reasons.append('cwd_scope')
        for fd in (process / 'fd').iterdir():
            try:
                if any(scope in os.readlink(fd) for scope in scopes):
                    reasons.append('fd_scope')
                    break
            except FileNotFoundError:
                pass
        executable = os.readlink(process / 'exe')
        name = Path(executable).name
        is_native = name.startswith('native-') or bool(re.fullmatch(r'(?:server|bark_sdk|barkd|bark)-[a-f0-9]{16}', name))
        root_reasons = []
        if any(scope in command for scope in root_scopes):
            root_reasons.append('argv_scope')
        if any(scope in os.readlink(process / 'cwd') for scope in root_scopes):
            root_reasons.append('cwd_scope')
        for fd in (process / 'fd').iterdir():
            try:
                if any(scope in os.readlink(fd) for scope in root_scopes):
                    root_reasons.append('fd_scope')
                    break
            except FileNotFoundError:
                pass
        controls = {'run.py', 'run.sh', 'server-cohort.py', 'cohort.py'}
        control_argument = any(Path(argument.decode(errors='replace')).name in controls for argument in argv[1:] if argument)
        financial_names = {'bitcoind', 'captaind', 'watchmand', 'bark', 'barkd', 'lightningd', 'cln-grpc', 'grpc', 'hold', 'postgres', 'postmaster', 'pg_ctl', 'cargo-nextest', 'nextest', 'just'}
        if root_reasons:
            if is_native or name in financial_names or control_argument:
                reasons += ['root_previous_financial_' + reason for reason in root_reasons]
            else:
                root_readonly_exempted.append({'pid': int(process.name), 'executable_name': name, 'reasons': sorted(set(root_reasons))})
        if is_native and b'--list' not in argv:
            native.append({'pid': int(process.name), 'executable_name': name})
        if reasons:
            actors.append({'pid': int(process.name), 'reasons': sorted(set(reasons))})
    except FileNotFoundError:
        pass
    except (OSError, ValueError, AttributeError, IndexError) as error:
        unreadable.append({'pid': int(process.name), 'error_kind': type(error).__name__})
shm = os.statvfs('/dev/shm')
disk = os.statvfs('/fresh-cache')
capacity_ok = shm.f_bavail * shm.f_frsize >= 256 * 1024**2 and disk.f_bavail * disk.f_frsize >= 20 * 1024**3
record = {'at': datetime.now(timezone.utc).isoformat(), 'state': 'PARENT_SCOPED_AND_CAPACITY_PASS' if not actors and not unreadable and len(native) <= 1 and capacity_ok else 'BLOCKED', 'scopes': scopes, 'root_previous_financial_scopes': root_scopes, 'root_previous_group_or_SID': 'UNKNOWN_NOT_RECORDED_BY_ROOT_RUNNER; scoped_process_checks_used', 'root_nonfinancial_scoped_processes_exempted': root_readonly_exempted, 'known_previous_groups_and_SIDs': sorted(known_groups), 'actors': actors, 'actor_count': len(actors), 'unreadable': unreadable, 'unreadable_count': len(unreadable), 'global_native_candidates': native, 'global_native_count_before_launch': len(native), 'global_max_after_one_launch': 2, 'minimum_SHM_bytes': 256 * 1024**2, 'minimum_disk_bytes': 20 * 1024**3, 'capacity_ok': capacity_ok, 'SHM_available_bytes': shm.f_bavail * shm.f_frsize, 'disk_available_bytes': disk.f_bavail * disk.f_frsize, 'probe_ancestry_excluded': sorted(excluded), 'scope_limit': 'Same-UID own/prior process argv/cwd/FD plus known groups; previous rootV16/V17 native/financial services/controllers must be gone. Root-authorized nonfinancial read-only audit parsers may coexist. No private file read/process action; root retains separate lane.'}
(root / 'parent-actor-admission.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps({key: record[key] for key in ['state', 'actor_count', 'unreadable_count', 'global_native_count_before_launch', 'SHM_available_bytes', 'disk_available_bytes']}))
assert not actors and not unreadable and len(native) <= 1 and capacity_ok, 'scoped actors, unreadable processes, or insufficient native lane capacity; preserve all state'
