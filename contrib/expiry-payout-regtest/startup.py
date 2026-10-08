"""Enabled startup checks the real watchman config; disabled mode ignores it."""
from common import *
import re

original=(F/'captaind.toml').read_text()
watch=(F/'watchmand.toml').read_text()
foreign=OUT/'foreign-watchmand.toml'
foreign.write_text(re.sub(r'(?m)^sweep_address = .*$', 'sweep_address = '+json.dumps(rpc('getnewaddress')),watch))

def rejected(text,message,label):
    path=OUT/(label+'.toml');path.write_text(text)
    args=[str(B/'target/debug/captaind'),'--config',str(path),'start']
    if NATIVE_IMAGE:
        args=['docker','run','--rm','--network','host','--user','1000:1000','-v',str(E)+':'+str(E),
            '--entrypoint','captaind',NATIVE_IMAGE,*args[1:]]
    result=cmd(args,label,check=False,timeout=30)
    assert result.returncode!=0 and message in result.stdout+result.stderr,(label,result.returncode,result.stderr)
    assert not listening(48535)

stop_daemon('captaind')
try:
    rejected(re.sub(r'(?m)^watchman_config = .*$', 'watchman_config = '+json.dumps(str(foreign)),original),
        'sweeps into the rounds wallet','foreign-sweep')
    rejected(original.replace('admin_address = "127.0.0.1:48536"','admin_address = "0.0.0.0:48536"'),
        'bound to loopback','nonlocal-admin')
    rejected(re.sub(r'(?m)^watchman_config = .*\n','',original),
        'require watchman_config','missing-watchman-config')
    # Configuration is intentionally unusable for payout policy, but disabled
    # startup must preserve ordinary captaind behavior and resume no new task.
    (F/'captaind.toml').write_text(re.sub(r'(?m)^watchman_config = .*\n','',original).replace('enabled = true','enabled = false'))
    start_daemon('captaind')
    before=q('SELECT count(*) FROM expiry_settlement')
    mine(1)
    assert q('SELECT count(*) FROM expiry_settlement')==before
    stop_daemon('captaind')
finally:
    (F/'captaind.toml').write_text(original)
    if not listening(48535):start_daemon('captaind')
ticks()
event('PASS',scenario='enabled rejects foreign sweep, missing watchman config and nonloopback admin; disabled startup remains usable')
