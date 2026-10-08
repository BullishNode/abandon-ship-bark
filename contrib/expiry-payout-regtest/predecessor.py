from common import *

configure_task(enabled=True)
import shutil

owner,coin=board('predecessor',150000)
input_count=int(os.environ.get('INPUT_COUNT','1'))
assert input_count in [1,2]
for _ in range(input_count-1):
    bark(owner,'board','150000 sat')
    mine(4)
inputs=bark(owner,'vtxos')
assert len(inputs)==input_count,inputs
backup=owner+'-before-refresh'
shutil.copytree(E/'wallets'/owner,E/'wallets'/backup)
bark(owner,'refresh','--delegated','--all')
round_id=wait(lambda:q(f"SELECT spent_in_round FROM vtxo WHERE vtxo_id='{coin['id']}'"),'delegated round')
rows=json.loads(q(f"SELECT json_agg(x) FROM (SELECT vtxo_id id,amount,expiry expiry_height,anchor_point chain_anchor FROM vtxo WHERE spent_in_round IS NULL AND spend_state='unclaimed' AND amount>1000 AND anchor_point LIKE (SELECT funding_txid||':%' FROM round WHERE id={round_id})) x"))
assert len(rows)==1,rows
replacement=rows[0]
mine(3)
offline=E/'wallets'/backup/'config.toml'
offline.write_text(offline.read_text().replace(':48535',':48534'))
bark(backup,'exit','start','--vtxo',coin['id'])
for _ in range(40):
    if q(f"SELECT confirmed_height IS NOT NULL FROM vtxo WHERE vtxo_id='{coin['id']}'")=='t': break
    bark(backup,'exit','progress')
    mine(1)
else: raise AssertionError('predecessor leaf did not confirm')
leaf=rpc('getrawtransaction',[coin['id'].split(':')[0],True])
assert leaf['confirmations']>=1
assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{replacement['id']}'")=='unclaimed'
expire([replacement])
bark(backup,'exit','progress')
bark(backup,'exit','claim',rpc('getnewaddress'),'--all','--no-sync')
mine(1)
claim=q(f"SELECT onchain_spent_txid FROM vtxo WHERE vtxo_id='{coin['id']}'")
assert claim and rpc('getrawtransaction',[claim,True])['confirmations']>=1
ticks()
r=row(replacement['id'])
save('predecessor-proof.json',dict(owner=owner,backup=backup,predecessors=inputs,replacement=replacement,leaf=leaf,payment=r))
if os.environ.get('EXPECT_PREDECESSOR_BUG')=='1':
    assert r,'expected pre-fix payout after a confirmed predecessor exit'
    proof=settle([replacement])[0]
    event('REPRODUCED',scenario='unfinished refresh predecessor exited and replacement paid',**proof)
else:
    assert r is None,'replacement must not be paid after its predecessor exited'
    event('PASS',scenario='unfinished refresh predecessor exit blocks replacement payment',input_count=input_count,
        partial_compensation='checked separately by mixed-cancel' if input_count==2 else 'not applicable')
