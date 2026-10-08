"""A replayed round attempt must not make an ordinary interactive refresh fail."""
from common import *
import re

settings=(F/'captaind.toml').read_text()
flow=os.environ.get('EXPIRY_REFRESH_BOUNDARY_FLOW','refresh')
assert flow in ['refresh','maintain']
proxy=None
try:
    configure_task(enabled=False)
    owner,coin=board('refresh-boundary',60000)
    log=OUT/'boundary-proxy.log'
    with log.open('w') as output:
        proxy=subprocess.Popen([ROOT/'target/debug/examples/expiry_round_boundary_proxy'],
            stdout=output,stderr=subprocess.STDOUT)
    def records():
        result=[]
        for line in log.read_text().splitlines():
            try:result.append(json.loads(line))
            except ValueError:pass
        return result
    def ready():
        assert proxy.poll() is None,'round proxy exited before readiness'
        return next((r['address'] for r in records() if r.get('event')=='ready'),None)
    address=wait(ready,'round boundary proxy')
    config=E/'wallets'/owner/'config.toml'
    text,n=re.subn(r'(?m)^server_address = .*$', 'server_address = '+json.dumps(address),config.read_text())
    assert n==1
    if flow=='maintain':
        text=re.sub(r'(?m)^vtxo_refresh_expiry_threshold = .*\n','',text)
        text='vtxo_refresh_expiry_threshold = 1000\n'+text
    config.write_text(text)
    before=time.monotonic()
    result=bark(owner,*(['refresh','--all'] if flow=='refresh' else ['maintain']),check=False)
    observed=records()
    replay=[r for r in observed if r.get('event')=='stale-attempt-replayed']
    assert len(replay)==1 and replay[0]['old_round']!=replay[0]['current_round'],observed
    attempts=[r for r in observed if r.get('event')=='submission-result']
    save('refresh-boundary-outcome.json',dict(client_image=IMAGE,flow=flow,owner=owner,input=coin,
        expected='one accepted real refresh despite a stale initial event',
        exit_code=result.returncode,seconds=time.monotonic()-before,proxy=observed))
    assert result.returncode==0,('refresh failed at the reproduced round boundary',result.stderr[-1800:])
    assert len(attempts)==1 and attempts[0]['accepted'],attempts
    status=json.loads(result.stdout) if flow=='refresh' else dict(funding_txid=q(
        f"SELECT funding_txid FROM round WHERE id=(SELECT spent_in_round FROM vtxo WHERE vtxo_id='{coin['id']}')"))
    assert status['funding_txid'],'maintenance did not create a round'
    mine(3)
    renewed=bark(owner,'vtxos')
    assert renewed and coin['id'] not in [c['id'] for c in renewed]
    assert q(f"SELECT spent_in_round IS NOT NULL FROM vtxo WHERE vtxo_id='{coin['id']}'")=='t'
    assert rpc('getrawtransaction',[status['funding_txid'],True])['confirmations']>=3
    assert row(coin['id']) is None
    save('refresh-boundary-proof.json',dict(status='PASS',client_image=IMAGE,flow=flow,
        input=coin,outputs=renewed,refresh=status,proxy=observed))
    event('PASS',scenario='stale replay skipped; fresh round refresh confirmed',txid=status['funding_txid'])
finally:
    if proxy is not None and proxy.poll() is None:
        proxy.send_signal(signal.SIGINT);proxy.wait(timeout=30)
    if listening(48535):stop_daemon('captaind')
    (F/'captaind.toml').write_text(settings);start_daemon('captaind')
