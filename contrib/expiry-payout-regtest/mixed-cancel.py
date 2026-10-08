"""A failed delegated exchange must not strand its unexited input."""
from common import *
import runpy

fixture=F/'mixed-cancel.json'
if os.environ.get('REUSE_MIXED_CANCEL')=='1':
    case=json.loads(fixture.read_text())
else:
    os.environ['INPUT_COUNT']='2'
    runpy.run_path(str(ROOT/'contrib/expiry-payout-regtest/predecessor.py'))
    case=json.loads((OUT/'predecessor-proof.json').read_text())
    fixture.write_text(json.dumps(case,indent=2)+'\n')
exited=[c for c in case['predecessors'] if c['id'].split(':')[0]==case['leaf']['txid']]
remaining=[c for c in case['predecessors'] if c not in exited]
assert len(exited)==len(remaining)==1
ticks(10)
assert row(remaining[0]['id']), 'swept original entitlement remains stranded in unfinished refresh'
assert row(exited[0]['id']) is None,'exited original also paid'
assert row(case['replacement']['id']) is None,'cancelled replacement also paid'
proof=settle(remaining)[0]
mine(3)
# Bind the actual chain output and receipt to the original owner's key/value.
original=remaining[0]
address=payout_address(original)
tx=rpc('getrawtransaction',[proof['txid'],True])
outputs=[o for o in tx['vout'] if o['scriptPubKey'].get('address')==address]
assert len(outputs)==1,outputs
out=outputs[0]
receipt=next(o for o in proof['receipt']['outputs'] if o['vout']==out['n'])
assert SAT(out['value'])==receipt['amount_sat']
assert receipt['amount_sat']+receipt['fee_sat']==original['amount_sat'],(receipt,original)
assert all(o['scriptPubKey'].get('address')!=payout_address(exited[0]) for o in tx['vout'])
assert q(f"SELECT count(*) FROM round_part_input WHERE vtxo_id='{remaining[0]['id']}'")=='0'
assert q(f"SELECT count(*) FROM expiry_cancelled_participation WHERE '{remaining[0]['id']}'=ANY(input_ids)")=='1'
ticks()
assert row(remaining[0]['id'])['txid']==proof['txid']
# Seed-only recovery finds and spends B after the cancelled replacement vanished.
seed=(E/'wallets'/case['owner']/'mnemonic').read_text()
desc=subprocess.run([str(ROOT/'target/debug/examples/coin_key_descriptor'),'regtest'],
    input=seed,text=True,capture_output=True,check=True).stdout.strip()
desc+='#'+rpc('getdescriptorinfo',[desc])['checksum']
scan=rpc('scantxoutset',['start',[dict(desc=desc,range=[0,200])]])
found=[o for o in scan['unspents'] if o['txid']==proof['txid'] and o['vout']==out['n']]
assert len(found)==1,found
recovery='cancel-seed-'+str(time.time_ns())
rpc('createwallet',[recovery,False,True])
assert all(r['success'] for r in rpc('importdescriptors',[[dict(desc=desc,range=[0,200],timestamp=0)]],wallet=recovery))
rate=rpc('estimatesmartfee',[6])['feerate']*1e5
spend=rpc('sendall',dict(recipients=[rpc('getnewaddress')],fee_rate=round(rate,3)),wallet=recovery)['txid']
mine(1)
spent=rpc('getrawtransaction',[spend,True])
assert spent['confirmations']>=1
assert any(i['txid']==proof['txid'] and i['vout']==out['n'] for i in spent['vin'])
save('mixed-cancel-proof.json',dict(case=case,released_payment=proof,recipient_address=address,
    seed_discovered=found,recovery_spend=spend))
event('PASS',scenario='failed mixed delegated refresh retires replacement and settles only swept original',**proof)
