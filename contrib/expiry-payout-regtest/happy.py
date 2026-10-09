from common import *

owner,coin=board('native-task-happy',120000)
assert row(coin['id']) is None, 'paid before expiry'
expire([coin])
wait(lambda:row(coin['id']), 'native task payout')
proof=wait(lambda: payment(coin['id']) if (F/'receipts'/(row(coin['id'])['txid']+'.json')).exists() else None, 'native fee receipt')
tx=rpc('getrawtransaction',[proof['txid'],True])
recipient=[o for o in tx['vout'] if o['scriptPubKey'].get('address')==payout_address(coin)]
assert len(recipient)==1
charged=next(o['fee_sat'] for o in proof['receipt']['outputs'] if o['vout']==recipient[0]['n'])
assert SAT(recipient[0]['value'])+charged==coin['amount_sat']
# Ticks run without any sidecar or Exchange call.
time.sleep(3)
assert row(coin['id'])['txid']==proof['txid']
assert q(f"SELECT count(*) FROM expiry_settlement WHERE id='{coin['id']}'")=='1'
mine(3)
assert rpc('getrawtransaction',[proof['txid'],True])['confirmations']>=3
assert q(f"SELECT confirmed_at_height IS NOT NULL FROM nursery_tx WHERE txid='{proof['txid']}'")=='t'
save('happy-proof.json',dict(wallet=owner,coin=coin,proof=proof))
event('PASS',scenario='native task pays autonomously, exact fee, nursery confirmation',**proof)
