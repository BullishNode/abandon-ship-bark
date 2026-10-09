from common import *

configure_task(enabled=False)

owner,coin=board('participating',100000)
expire([coin])
# Immediate refresh remains available after expiry. Explicit scheduling beyond
# expiry is rejected by captaind and cannot produce an in-flight test case.
bark(owner,'refresh','--delegated','--all')
wait(lambda:q(f"SELECT spent_in_round IS NOT NULL FROM vtxo WHERE vtxo_id='{coin['id']}'")=='t','owner refresh enters a round')
pending=q(f"SELECT count(*) FROM round_participation p JOIN round_part_input i ON i.participation_id=p.id WHERE i.vtxo_id='{coin['id']}' AND p.round_id IS NOT NULL AND p.forfeited_at IS NULL")
assert pending=='1',pending
assert row(coin['id']) is None
configure_task(enabled=True)
ticks()
assert row(coin['id']) is None
mine(3)
replacement=bark(owner,'vtxos')[0]
assert replacement['id']!=coin['id']
expire([replacement]);ticks()
proof=settle([replacement])[0]
assert row(coin['id']) is None
save('participation-proof.json',dict(original=coin,replacement=replacement,payment=proof))
event('PASS',scenario='expired owner refresh wins; unfinished round input is not paid; replacement settles once',txid=proof['txid'])
