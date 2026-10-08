from common import *

configure_task(enabled=False)

_,large=board('funding-large',180000)
_,small=board('funding-small',50000)
expire([large,small]);mine(6)
address=(F/'sweep-address.txt').read_text().strip()
dest=rpc('getnewaddress')
# Explicit private-test funding outage: move existing rounds capital temporarily
# into the faucet, then return it fragmented. This is not an automatic top-up.
stop_daemon('captaind')
try:
    result=cmd([B/'target/debug/captaind','--config',F/'captaind.toml','drain',dest],'drain-fixture')
    drain=result.stdout.strip().splitlines()[-1]
    raw=rpc('getrawtransaction',[drain,True])
    capital=sum(SAT(o['value']) for o in raw['vout'] if o['scriptPubKey'].get('address')==dest)
    assert capital>500000
    rpc('generatetoaddress',[3,rpc('getnewaddress')])
finally:start_daemon('captaind')
synced()
configure_task(enabled=True)
ticks()
assert row(small['id']) is None
fragments=[rpc('sendtoaddress',[address,.00005,'','',False,True,None,'unset',None,3]) for _ in range(20)]
mine(3)
assert all(rpc('getrawtransaction',[t,True])['confirmations']>=3 for t in fragments)
ticks()
assert row(large['id']) is None,'underfunded large entitlement was claimed'
proof=settle([small])[0]
tx=rpc('getrawtransaction',[proof['txid'],True])
assert len(tx['vin'])>=5,'test did not exercise fragmented selection'
mine(3)
# Return the rest of precisely the withdrawn capital. The faucet pays only
# this test's temporary transfer fees; no additional rounds capital is minted.
returned=rpc('sendtoaddress',[address,(capital-100000)/1e8,'','',False,True,None,'unset',None,3])
mine(3)
ticks();mine(3)
large_proof=settle([large])[0]
assert rpc('getrawtransaction',[large_proof['txid'],True])['confirmations']>=3
save('fragmented-proof.json',dict(withdrawn=drain,withdrawn_sat=capital,fragments=fragments,
    returned_remainder=returned,fragmented_payment=proof,input_count=len(tx['vin']),recovered_payment=large_proof))
event('PASS',scenario='funding outage preserves entitlements; fragmented smaller payout progresses; return restores large payout',
    input_count=len(tx['vin']),small=proof['txid'],large=large_proof['txid'])
