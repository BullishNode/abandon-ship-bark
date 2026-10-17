import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time

root = Path('/fresh-cache/testing-track-20261017/batch17-upstream-core-default')
command = ['just', '--set', 'JUSTFILE_DIR', str(root), 'int-binaries',
    str(root / 'nextest-binaries-core-runtime-immutable.json'),
    '--test-threads 1 --retries 0 --config-file ' + str(root / 'nextest.toml')]
with (root / 'nextest.log').open('wb') as log:
    child = subprocess.Popen(command, cwd='/home/francis/abandon-ship-fresh-integration',
        stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    (root / 'owned-process.json').write_text(json.dumps({
        'runner_pid': os.getpid(), 'child_pid': child.pid, 'owned_pgid': child.pid,
        'command': command, 'ownership': 'Dedicated new process group created by this runner'}, indent=2) + '\n')
    resource_error = None
    while child.poll() is None:
        text = (root / 'nextest.log').read_text(errors='replace')
        match = re.search(r'No space left on device|could not resize shared memory|Cannot allocate memory|'
            r'Resource temporarily unavailable|Too many open files|could not fork|'
            r'failed to spawn[^\n]*os error (?:11|12|24|28)', text, re.IGNORECASE)
        if match:
            resource_error = match.group(0)
            (root / 'resource-stop.json').write_text(json.dumps({
                'state': 'STOPPED_ON_RESOURCE_FAILURE', 'matched_error': resource_error,
                'signalled_owned_pgid': child.pid, 'retry': False}, indent=2) + '\n')
            os.killpg(child.pid, signal.SIGINT)
            try:
                child.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGTERM)
                child.wait(timeout=20)
            break
        time.sleep(0.5)
    code = child.wait()
(root / 'runner-result.json').write_text(json.dumps({
    'exit_code': code, 'resource_failure': resource_error,
    'retries': 0, 'command': command}, indent=2) + '\n')
raise SystemExit(101 if resource_error else code)
