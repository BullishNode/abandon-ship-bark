from common import *
from concurrent.futures import ThreadPoolExecutor

configure_task(enabled=True)

# Twelve leaves give multiple internal sibling branches in the radix-4 tree.
wallets=[newwallet('partial'+str(i),0) for i in range(12)]
addresses=[bark(w,'onchain','address')['address'] for w in wallets]
rpc('sendmany',dict(dummy='',amounts={a:.002 for a in addresses},fee_rate=3))
mine(3)
with ThreadPoolExecutor(12) as pool:
    list(pool.map(lambda w:bark(w,'board','60000 sat'),wallets))
mine(3)
originals=[bark(w,'vtxos')[0] for w in wallets]
# Schedule all registrations before advancing the chain. Concurrent interactive
# CLI launches can straddle two round windows and would test unrelated trees.
scheduled=rpc('getblockcount')+2
for w in wallets:bark(w,'refresh','--delegated','--all','--height',str(scheduled))
mine(2)
for c in originals:
    wait(lambda:q(f"SELECT spent_in_round IS NOT NULL FROM vtxo WHERE vtxo_id='{c['id']}'")=='t','scheduled refresh funded')
mine(3)
coins=[bark(w,'vtxos')[0] for w in wallets]
assert len({c['chain_anchor'] for c in coins})==1,'fixture did not join one round'
mine(3)

def path(coin):
    raw=q(f"SELECT encode(vtxo,'hex') FROM vtxo WHERE vtxo_id='{coin['id']}'")
    return cmd([B/'target/debug/examples/vtxo_path'],'path',input=raw).stdout.strip().splitlines()

paths={c['id']:path(c) for c in coins}
save('coins-paths.json',dict(coins=coins,paths=paths))
bark(wallets[0],'exit','start','--vtxo',coins[0]['id'])
for _ in range(40):
    if q(f"SELECT confirmed_height IS NOT NULL FROM vtxo WHERE vtxo_id='{coins[0]['id']}'")=='t':break
    bark(wallets[0],'exit','progress')
    mine(1)
else:raise AssertionError('exit leaf did not confirm')

groups={}
for c in coins[1:]:
    if q(f"SELECT confirmed_height IS NULL FROM vtxo WHERE vtxo_id='{c['id']}'")!='t':continue
    for op in paths[c['id']][1:-1]:
        txid,index=op.split(':')
        if rpc('gettxout',[txid,int(index)]) is not None:
            groups.setdefault(op,[]).append(c)
            break
assert len(groups)>=2,groups
held,paid=list(groups)[:2]
assert held not in paths[groups[paid][0]['id']]
assert paid not in paths[groups[held][0]['id']]
# Fault: delay watchman's discovery of one actual unspent internal output.
# Only its frontier scheduling is delayed; no chain spend or receipt is invented.
stop_daemon('watchmand')
q(f"UPDATE vtxo SET frontier_at=NULL,updated_at=NOW() WHERE vtxo_id='{held}'")
start_daemon('watchmand')
try:
    mine(max(0,max(c['expiry_height'] for c in coins)+145-rpc('getblockcount')))
    for _ in range(60):
        if q(f"SELECT onchain_spent_txid IS NOT NULL FROM vtxo WHERE vtxo_id='{paid}'")=='t':break
        time.sleep(.3);mine(1)
    else:raise AssertionError('eligible sibling was not swept')
    mine(6)
    held_txid,held_index=held.split(':')
    assert rpc('gettxout',[held_txid,int(held_index)]) is not None,'delayed branch unexpectedly spent'
    swept=q(f"SELECT onchain_spent_txid FROM vtxo WHERE vtxo_id='{paid}'")
    raw=rpc('getrawtransaction',[swept,True])
    assert raw['confirmations']>=6
    assert any(i['txid']+':'+str(i['vout'])==paid for i in raw['vin'])
    assert all(i['txid']+':'+str(i['vout'])!=held for i in raw['vin'])
    prime()
    settle(groups[paid])
    ticks()
    assert all(row(c['id']) is None for c in groups[held]),'sibling sweep paid delayed branch'
    proofs=[payment(c['id']) for c in groups[paid]]
    assert row(coins[0]['id']) is None,'exited leaf paid again'
    save('partial-first-proof.json',dict(held_outpoint=held,held_coins=groups[held],swept_outpoint=paid,
        sweep=swept,paid=proofs,exited=coins[0]))
finally:
    stop_daemon('watchmand')
    q(f"UPDATE vtxo SET frontier_at=NOW(),updated_at=NOW() WHERE vtxo_id='{held}'")
    start_daemon('watchmand')
for _ in range(60):
    if q(f"SELECT onchain_spent_txid IS NOT NULL FROM vtxo WHERE vtxo_id='{held}'")=='t':break
    time.sleep(.3);mine(1)
else:raise AssertionError('released frontier did not sweep')
mine(6)
save('partial-released-proof.json',settle(groups[held]))
event('PASS',scenario='exact partial branches: sibling sweep waits, own sweep pays, exited leaf excluded',
    delayed=held,swept=paid,groups={k:[c['id'] for c in v] for k,v in groups.items()})
