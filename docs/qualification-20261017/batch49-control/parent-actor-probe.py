import json, os, re
from pathlib import Path
from datetime import datetime, timezone

assert os.environ.get('IN_NIX_SHELL')
assert 'BARK_DOUBLE_DRIVE_ACTIONS' not in os.environ
root = Path('/fresh-cache/testing-track-20261017/batch49-default-daemon-singleton-v11c')
labels = ['batch45-recovery-three-v10', 'batch47-ordinary-recovery-three-v10', 'batch46-lightning-physical-backup-v11c', 'batch49-default-daemon-singleton-v11c']
scopes = [prefix + label for prefix in ['/fresh-cache/testing-track-20261017/', '/home/francis/.cache/abandon-fresh/testing-track-20261017/'] for label in labels]
scopes += ['/fresh-cache/independent-two-v11c-20261018', '/home/francis/.cache/abandon-fresh/independent-two-v11c-20261018']
known_groups = {145878, 146890, 148754, 154295}
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
record = {'at': datetime.now(timezone.utc).isoformat(), 'state': 'PARENT_SCOPED_AND_CAPACITY_PASS' if not actors and not unreadable and len(native) <= 1 else 'BLOCKED', 'scopes': scopes, 'known_previous_groups_and_SIDs': sorted(known_groups), 'actors': actors, 'actor_count': len(actors), 'unreadable': unreadable, 'unreadable_count': len(unreadable), 'global_native_candidates': native, 'global_native_count_before_launch': len(native), 'global_max_after_one_launch': 2, 'SHM_available_bytes': shm.f_bavail * shm.f_frsize, 'disk_available_bytes': disk.f_bavail * disk.f_frsize, 'probe_ancestry_excluded': sorted(excluded), 'scope_limit': 'Same-UID proc argv/cwd/FD and known old groups; no private file read or process action. Root retains one separate lane.'}
(root / 'parent-actor-admission.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps({key: record[key] for key in ['state', 'actor_count', 'unreadable_count', 'global_native_count_before_launch', 'SHM_available_bytes', 'disk_available_bytes']}))
assert not actors and not unreadable and len(native) <= 1, 'scoped actors, unreadable processes, or insufficient native lane capacity; preserve all state'
