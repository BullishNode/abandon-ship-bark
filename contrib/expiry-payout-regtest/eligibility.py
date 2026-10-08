from common import *

configure_task(enabled=True)
owner,coin=board('eligibility',90000)
ticks()
assert row(coin['id']) is None,'nonexpired coin paid'
configure_task(enabled=False)
try:
    expire([coin])
    offboard=bark(owner,'offboard','--vtxo',coin['id'],'--no-sync')
    spent=q(f"SELECT offboarded_in FROM vtxo WHERE vtxo_id='{coin['id']}'")
    assert spent and rpc('getrawtransaction',[spent,True])
    assert row(coin['id']) is None
    mine(3)
finally:configure_task(enabled=True)
ticks()
assert row(coin['id']) is None
save('eligibility-proof.json',dict(coin=coin,offboard_txid=spent))
event('PASS',scenario='nonexpired excluded; expired owner offboard wins before task, remains excluded',coin=coin['id'],offboard=spent)
