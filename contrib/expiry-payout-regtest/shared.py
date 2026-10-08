from common import *

configure_task(enabled=False)
receiver=newwallet('shared-receiver',0)
address=cmd(bark_args(receiver,'address'),'ark-address').stdout.strip()
for i in range(3):
    sender,_=board('shared-sender'+str(i),60000)
    bark(sender,'send',address,'20000 sat')
coins=bark(receiver,'vtxos')
assert len(coins)==3 and len({c['user_pubkey'] for c in coins})==1
expire(coins)
configure_task(enabled=True)
proofs=settle(coins)
assert len({p['txid'] for p in proofs})==1,'shared eligible batch split unexpectedly'
proof=proofs[0]
tx=rpc('getrawtransaction',[proof['txid'],True])
outputs=[(i,o) for i,o in enumerate(tx['vout']) if o['scriptPubKey'].get('address')==payout_address(coins[0])]
assert len(outputs)==1
index,out=outputs[0]
receipt=[o for o in proof['receipt']['outputs'] if o['vout']==index][0]
assert SAT(out['value'])==receipt['amount_sat']
assert receipt['amount_sat']+receipt['fee_sat']==sum(c['amount_sat'] for c in coins)
ticks();mine(3)
assert all(row(c['id'])['txid']==proof['txid'] for c in coins)
save('shared-proof.json',dict(coins=coins,payment=proof))
event('PASS',scenario='three same-key Ark transfers merge into one exact fee-bearing payout output',txid=proof['txid'])
