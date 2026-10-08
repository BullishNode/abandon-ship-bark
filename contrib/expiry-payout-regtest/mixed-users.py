"""Real transfers, refresh, offboard, exit and abandonment in one population."""
from common import *
from concurrent.futures import ThreadPoolExecutor

configure_task(enabled=True)
names=['sender','receiver','offboarder','last-recipient','exiter','abandoner']
wallets={name:newwallet('mixed-'+name,200000 if name in ['sender','exiter','abandoner'] else 0)
    for name in names}
mine(3)
amounts={'sender':120000,'exiter':25000,'abandoner':40000}
with ThreadPoolExecutor(3) as pool:
    list(pool.map(lambda name:bark(wallets[name],'board',str(amounts[name])+' sat'),amounts))
mine(3)
boards={name:bark(wallets[name],'vtxos')[0] for name in amounts}
transfers=[]
for sender,receiver,amount in [('sender','receiver',30000),('sender','offboarder',20000),
                              ('receiver','last-recipient',10000)]:
    inputs=bark(wallets[sender],'vtxos');assert inputs
    address=bark(wallets[receiver],'address',check=False)
    assert address.returncode==0,address.stderr
    bark(wallets[sender],'send',address.stdout.strip(),str(amount)+' sat')
    received=bark(wallets[receiver],'vtxos')
    assert sum(c['amount_sat'] for c in received)==amount
    remaining=bark(wallets[sender],'vtxos')
    remaining_ids={c['id'] for c in remaining}
    selected=[c for c in inputs if c['id'] not in remaining_ids]
    retained=[c for c in inputs if c['id'] in remaining_ids]
    assert selected,'send did not consume an input'
    assert sum(c['amount_sat'] for c in inputs)==amount+sum(c['amount_sat'] for c in remaining)
    spent=[q(f"SELECT oor_spent_txid FROM vtxo WHERE vtxo_id='{c['id']}' AND spend_state='spent'") for c in selected]
    assert all(spent),spent
    assert all(q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{c['id']}'")=='spendable' for c in retained)
    transfers.append(dict(sender=sender,receiver=receiver,amount_sat=amount,
        input_ids=[c['id'] for c in selected],retained_ids=[c['id'] for c in retained],
        received=received,remaining=remaining,spend_txids=spent))
save('transfers.json',transfers)

# One recipient renews while another leaves through a cooperative offboard.
old=bark(wallets['receiver'],'vtxos');assert old
bark(wallets['receiver'],'refresh','--delegated','--all')
wait(lambda:all(q(f"SELECT spent_in_round IS NOT NULL FROM vtxo WHERE vtxo_id='{c['id']}'")=='t'
    for c in old),'transferred recipient enters refresh')
mine(3)
renewed=bark(wallets['receiver'],'vtxos')
assert renewed and {c['id'] for c in old}.isdisjoint(c['id'] for c in renewed)
offboard_coins=bark(wallets['offboarder'],'vtxos');assert offboard_coins
offboard=bark(wallets['offboarder'],'offboard','--all')
mine(3)
assert rpc('getrawtransaction',[offboard['offboard_txid'],True])['confirmations']>=3

exited=boards['exiter'];bark(wallets['exiter'],'exit','start','--vtxo',exited['id'])
for _ in range(40):
    if q(f"SELECT confirmed_height IS NOT NULL FROM vtxo WHERE vtxo_id='{exited['id']}'")=='t':break
    bark(wallets['exiter'],'exit','progress');mine(1)
else:raise AssertionError('mixed user exit did not confirm')

eligible=[]
for name in ['sender','receiver','last-recipient','abandoner']:
    coins=bark(wallets[name],'vtxos');assert coins,name
    eligible.extend(coins)
assert len({c['id'] for c in eligible})==len(eligible)
excluded={id for transfer in transfers for id in transfer['input_ids']}
excluded.update(c['id'] for c in old+offboard_coins+[exited])
assert excluded.isdisjoint(c['id'] for c in eligible)
expire(eligible)
proofs=settle(eligible);mine(3)
outputs={}
for coin,proof in zip(eligible,proofs):
    key=(proof['txid'],payout_address(coin))
    outputs[key]=outputs.get(key,0)+coin['amount_sat']
for (txid,address),gross in outputs.items():
    tx=rpc('getrawtransaction',[txid,True])
    matching=[o for o in tx['vout'] if o['scriptPubKey'].get('address')==address]
    assert len(matching)==1
    output=matching[0]
    receipt=json.loads((F/'receipts'/(txid+'.json')).read_text())
    fee=next(o['fee_sat'] for o in receipt['outputs'] if o['vout']==output['n'])
    assert SAT(output['value'])+fee==gross
assert all(row(id) is None for id in excluded),'an earlier or otherwise settled entitlement was paid'
ticks()
assert {c['id']:row(c['id'])['txid'] for c in eligible}=={p['id']:p['txid'] for p in proofs}
save('mixed-user-proof.json',dict(status='PASS',wallets=wallets,boards=boards,
    transfers=transfers,refreshed_inputs=old,offboard=offboard,exit=exited,
    eligible=eligible,excluded=sorted(excluded),payments=proofs))
event('PASS',scenario='transfer chain, recipient refresh, offboard, exit and remaining-owner expiry',
    wallets=len(wallets),transfers=len(transfers),paid=len(eligible),excluded=len(excluded))
