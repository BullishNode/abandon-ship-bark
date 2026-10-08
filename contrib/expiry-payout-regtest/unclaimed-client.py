"""Restore an unclaimed expiry payment using the same barkd API as bark-web."""
from common import *
import urllib.error

configure_task(enabled=True)
birthday=rpc('getblockcount')
owner,original=board('unclaimed-client',60000)
bark(owner,'refresh','--delegated','--all')
rid=wait(lambda:q(f"SELECT spent_in_round FROM vtxo WHERE vtxo_id='{original['id']}'"),'delegated round')
coins=json.loads(q(f"SELECT json_agg(x) FROM (SELECT vtxo_id id,amount amount_sat,expiry expiry_height,anchor_point chain_anchor FROM vtxo WHERE spend_state='unclaimed' AND amount>1000 AND anchor_point LIKE (SELECT funding_txid||':%' FROM round WHERE id={rid})) x"))
assert len(coins)==1
coin=coins[0]
raw=q(f"SELECT encode(vtxo,'hex') FROM vtxo WHERE vtxo_id='{coin['id']}'")
coin.update(json.loads(cmd([ROOT/'target/debug/examples/vtxo_path','--json'],'decode-unclaimed',input=raw).stdout))
expire([coin]);proof=settle([coin])[0];mine(3)
source_tx=rpc('getrawtransaction',[proof['txid'],True])
expected=next(o for o in source_tx['vout'] if o['scriptPubKey'].get('address')==payout_address(coin))
assert rpc('gettxout',[proof['txid'],expected['n']]) is not None
save('expected-client-payout.json',dict(owner=owner,original=original,coin=coin,payment=proof,vout=expected['n'],birthday=birthday))
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
name='abandon-captaind-task-recovery-'+str(time.time_ns())
directory=OUT/'restored-wallet';directory.mkdir(mode=0o700)
cmd(['docker','run','-d','--name',name,'--network','host','--user','1000:1000',
    '-v',str(directory)+':/wallet','--entrypoint','barkd',IMAGE,'--datadir','/wallet',
    '--host','127.0.0.1','--port',str(port),'--no-auth'],'start-restored-barkd')
def api(path,data=None):
    request=urllib.request.Request(f'http://127.0.0.1:{port}/api/v1/'+path,
        json.dumps(data).encode() if data is not None else None,{'Content-Type':'application/json'})
    try:
        with urllib.request.urlopen(request,timeout=180) as response:
            body=response.read()
            return response.status,json.loads(body) if body else None
    except urllib.error.HTTPError as error:return error.code,error.read().decode()
try:
    wait(lambda:listening(port),'restored barkd listening')
    status,body=api('wallet/create',dict(network='regtest',ark_server='http://127.0.0.1:48535',
        chain_source={'bitcoind':{'bitcoind':'http://127.0.0.1:53443','bitcoind_auth':{'user-pass':{'user':'second','pass':'ark'}}}},
        mnemonic=(E/'wallets'/owner/'mnemonic').read_text().strip(),birthday_height=birthday))
    assert status==200,(status,body)
    observations=[]
    def discovered():
        status,body=api('wallet/sync',{});assert status==200,(status,body)
        status,known=api('wallet/vtxos?all=true');assert status==200,(status,known)
        # Match bark-web: adopt only expired spendable IDs returned by barkd.
        # Never inject the expected replacement ID from the server-side fixture.
        expired=[v['id'] for v in known if v['state']['type']=='spendable' and v['expiry_height']<=rpc('getblockcount')]
        if expired:
            status,body=api('wallet/vtxos/adopt-server-status',{'vtxo_ids':expired})
            assert status==200,(status,body)
        status,payouts=api('wallet/vtxos/expiry-payouts',{})
        observations.append(dict(status=status,payouts=payouts,known_vtxos=known))
        save('restored-client-payouts.json',observations)
        if status==200 and any(p['txid']==proof['txid'] and p['vout']==expected['n'] for p in payouts):return True
        time.sleep(5)
        return False
    wait(discovered,'restored barkd missed native unclaimed payout',90)
    status,spend=api('onchain/sweep-expiry-payouts',{})
    assert status==200,(status,spend)
    tx=rpc('getrawtransaction',[spend['txid'],True])
    assert any(i['txid']==proof['txid'] and i['vout']==expected['n'] for i in tx['vin'])
    mine(1)
    assert rpc('getrawtransaction',[spend['txid'],True])['confirmations']>=1
    save('restored-client-proof.json',dict(status='PASS',payout=proof['txid'],spend=spend['txid'],image=IMAGE))
    event('PASS',scenario='restored barkd discovers and spends unclaimed native payout',payout=proof['txid'],spend=spend['txid'])
finally:
    cmd(['docker','logs',name],'restored-barkd-log')
    cmd(['docker','stop',name],'stop-restored-barkd')
