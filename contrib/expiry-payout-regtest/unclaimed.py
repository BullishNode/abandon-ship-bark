from common import *

configure_task(enabled=True)

owner,coin=board('unclaimed',150000)
input_count=int(os.environ.get('INPUT_COUNT','1'))
assert input_count in [1,2]
for _ in range(input_count-1):
    bark(owner,'board','150000 sat')
    mine(4)
inputs=bark(owner,'vtxos')
assert len(inputs)==input_count,inputs
bark(owner,'refresh','--delegated','--all')
round_id=wait(lambda:q(f"SELECT spent_in_round FROM vtxo WHERE vtxo_id='{coin['id']}'"),'delegated round')
rows=json.loads(q(f"SELECT json_agg(x) FROM (SELECT vtxo_id id,amount,expiry expiry_height,anchor_point chain_anchor FROM vtxo WHERE spent_in_round IS NULL AND spend_state='unclaimed' AND amount>1000 AND anchor_point LIKE (SELECT funding_txid||':%' FROM round WHERE id={round_id})) x"))
assert len(rows)==1,rows
replacement=rows[0]
mine(3)
expire([replacement])
ticks()
proof=settle([replacement])[0]
assert q(f"SELECT confirmed_height IS NULL FROM vtxo WHERE vtxo_id='{coin['id']}'")=='t'
mine(3)
# A clean Core descriptor wallet knows only the mnemonic, not captaind records,
# Bark's database, round outputs, or the sidecar's receipts.
mnemonic=(E/'wallets'/owner/'mnemonic').read_text()
p=subprocess.run([str(ROOT/'target/debug/examples/coin_key_descriptor'),'regtest'],input=mnemonic,text=True,capture_output=True,check=True)
descriptor=p.stdout.strip()
checksum=rpc('getdescriptorinfo',[descriptor])['checksum']
descriptor += '#'+checksum
start=time.monotonic()
scan=rpc('scantxoutset',['start',[dict(desc=descriptor,range=[0,200])]])
found=[u for u in scan['unspents'] if u['txid']==proof['txid']]
assert len(found)==1,found
elapsed=time.monotonic()-start
recovery='seed-'+str(time.time_ns())
rpc('createwallet',[recovery,False,True])
result=rpc('importdescriptors',[[dict(desc=descriptor,range=[0,200],timestamp=0,active=False)]],wallet=recovery)
assert all(r['success'] for r in result),result
rate=rpc('estimatesmartfee',[6])['feerate']*1e5
spend=rpc('sendall',dict(recipients=[rpc('getnewaddress')],fee_rate=round(rate,3)),wallet=recovery)['txid']
mine(1)
assert rpc('getrawtransaction',[spend,True])['confirmations']>=1
save('unclaimed-seed-proof.json',dict(proof=proof,predecessors=inputs,replacement=replacement,
    recovery_spend=spend,scan_range=[0,200],scan_seconds=elapsed,seed_only_discovered=found))
event('PASS',scenario='legitimate unclaimed payment and seed-only Core discovery/spend',input_count=input_count,payout=proof['txid'],recovery_spend=spend,scan_seconds=elapsed)
