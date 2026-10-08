"""Restore and spend a real native payout through generated Python/UniFFI bindings."""
from common import *
import asyncio,hashlib,inspect

bindings=Path(os.environ['EXPIRY_NATIVE_BINDINGS']).resolve()
for name in ['bark.py','libbark_ffi.so']:assert (bindings/name).is_file(),name
save('bindings.json',dict(directory=str(bindings),files={name:hashlib.sha256(
    (bindings/name).read_bytes()).hexdigest() for name in ['bark.py','libbark_ffi.so']}))
sys.path.insert(0,str(bindings))
import bark as native

configure_task(enabled=True)
birthday=rpc('getblockcount')
owner,original=board('native-client',60000)
bark(owner,'refresh','--delegated','--all')
rid=wait(lambda:q(f"SELECT spent_in_round FROM vtxo WHERE vtxo_id='{original['id']}'"),'native client delegated round')
coins=json.loads(q(f"SELECT json_agg(x) FROM (SELECT vtxo_id id,amount amount_sat,expiry expiry_height,anchor_point chain_anchor FROM vtxo WHERE spend_state='unclaimed' AND amount>1000 AND anchor_point LIKE (SELECT funding_txid||':%' FROM round WHERE id={rid})) x"))
assert len(coins)==1
coin=coins[0]
raw=q(f"SELECT encode(vtxo,'hex') FROM vtxo WHERE vtxo_id='{coin['id']}'")
coin.update(json.loads(cmd([ROOT/'target/debug/examples/vtxo_path','--json'],'decode-native-unclaimed',input=raw).stdout))
expire([coin]);proof=settle([coin])[0];mine(3)
tx=rpc('getrawtransaction',[proof['txid'],True])
output=next(o for o in tx['vout'] if o['scriptPubKey'].get('address')==payout_address(coin))
fee=next(o['fee_sat'] for o in proof['receipt']['outputs'] if o['vout']==output['n'])
save('expected-native-payout.json',dict(original=original,coin=coin,payment=proof,
    outpoint=[proof['txid'],output['n']],amount_sat=SAT(output['value']),fee_sat=fee,birthday=birthday))

public=OUT/'public';public.mkdir()
receipt=public/(proof['txid']+'.json')
receipt.write_bytes((F/'receipts'/receipt.name).read_bytes())
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
config=OUT/'ark.conf'
config.write_text(f'events {{}}\nhttp {{ server {{ listen 127.0.0.1:{port}; http2 on; location /expiry-payouts/ {{ alias /receipts/; default_type application/json; }} location / {{ grpc_pass grpc://127.0.0.1:48535; }} }} }}\n')
container=PROJECT+'-native-client-'+str(time.time_ns())
proxy_image='nginx@sha256:97d490c12ba55b4946b01546d1c3ed324e8d41ab1c9fcb2a616aa470620e5b46'
cmd(['docker','run','-d','--rm','--name',container,'--network','host',
    '-v',str(config)+':/etc/nginx/nginx.conf:ro','-v',str(public)+':/receipts:ro',proxy_image],'native-receipt-proxy')

async def journey():
    parameters={key:None for key in inspect.signature(native.Config).parameters}
    parameters.update(server_address=f'http://127.0.0.1:{port}',bitcoind_address='http://127.0.0.1:53443',
        bitcoind_user='second',bitcoind_pass='ark')
    cfg=native.Config(**parameters)
    directory=OUT/'restored-native-wallet';directory.mkdir(mode=0o700)
    words=(E/'wallets'/owner/'mnemonic').read_text().strip()
    onchain=await native.OnchainWallet.default(native.Network.REGTEST,words,cfg,str(directory))
    await onchain.initial_scan(birthday)
    wallet=await native.Wallet.open(native.Network.REGTEST,words,cfg,
        native.WalletOpenArgs(datadir=str(directory),onchain=onchain,run_daemon=False))
    del words
    observations=[]
    for _ in range(18):
        await wallet.sync()
        known=await wallet.spendable_vtxos()
        if known:await wallet.adopt_server_vtxo_status([v.id for v in known])
        found=await wallet.find_expiry_payouts()
        observations.append([vars(p) for p in found]);save('native-discovery.json',observations)
        matching=[p for p in found if p.txid==proof['txid'] and p.vout==output['n']]
        if matching:break
        await asyncio.sleep(5)
    assert len(matching)==1,'restored native wallet missed its actual unclaimed payout'
    payout=matching[0]
    assert payout.amount_sats==SAT(output['value']) and payout.fee_sats==fee,vars(payout)
    content=receipt.read_bytes();receipt.unlink()
    try:
        unknown=[p for p in await wallet.find_expiry_payouts() if p.txid==proof['txid'] and p.vout==output['n']]
        assert len(unknown)==1 and unknown[0].amount_sats==payout.amount_sats and unknown[0].fee_sats is None
    finally:receipt.write_bytes(content)
    sweep=await wallet.sweep_expiry_payouts(5)
    spent=rpc('getrawtransaction',[sweep.txid,True])
    assert len(spent['vin'])==1 and (spent['vin'][0]['txid'],spent['vin'][0]['vout'])==(proof['txid'],output['n'])
    assert not await wallet.find_expiry_payouts()
    mine(1)
    assert rpc('getrawtransaction',[sweep.txid,True])['confirmations']>=1
    await onchain.sync()
    save('native-client-proof.json',dict(status='PASS',generated_language='Python/UniFFI',
        source='real native unclaimed payout; fresh seed restore',payout=vars(payout),sweep=vars(sweep),
        checks=['exact payout and fee','receipt outage preserves amount with unknown fee',
            'exact payout spent once','spending transaction confirmed']))
    event('PASS',scenario='generated native binding restores and spends unclaimed expiry payout',
        payout=proof['txid'],spend=sweep.txid)

try:
    wait(lambda:listening(port),'native receipt proxy')
    asyncio.run(journey())
finally:
    cmd(['docker','logs',container],'native-receipt-proxy-log')
    cmd(['docker','stop',container],'stop-native-receipt-proxy')
