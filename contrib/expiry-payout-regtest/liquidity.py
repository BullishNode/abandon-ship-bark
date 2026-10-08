from common import *

configure_task(enabled=True)
from concurrent.futures import ThreadPoolExecutor

cycles=int(os.environ.get('CYCLES','10'))
wallet_pairs=[[newwallet(f'liquidity{i}-{j}',200000) for j in range(2)] for i in range(cycles)]
mine(6)
# Settle pre-existing eligible fixture coins before the accounting boundary.
for _ in range(3):ticks();mine(3)

def rounds_balance():
    info=json.loads(cmd([B/'target/debug/captaind','--config',F/'captaind.toml','rpc','wallet'],'wallet-status').stdout)
    return info['rounds']['total_balance']

def chain_fee(txid):
    tx=rpc('getrawtransaction',[txid,True])
    assert tx['confirmations']>=1,(txid,tx)
    return sum(SAT(rpc('getrawtransaction',[i['txid'],True])['vout'][i['vout']]['value']) for i in tx['vin'])-sum(SAT(o['value']) for o in tx['vout'])

ledger=[]
initial=rounds_balance()
for cycle in range(cycles):
    wallets=wallet_pairs[cycle]
    start=rounds_balance()
    rid=int(q('SELECT coalesce(max(id),0) FROM round'))
    payment_ids=set(q('SELECT DISTINCT txid FROM expiry_settlement').splitlines())
    with ThreadPoolExecutor(2) as pool:
        list(pool.map(lambda w:bark(w,'board','60000 sat'),wallets))
    mine(3)
    boards=[bark(w,'vtxos')[0] for w in wallets]
    inflow=0
    for c in boards:
        txid,vout=c['chain_anchor'].split(':')
        inflow+=SAT(rpc('getrawtransaction',[txid,True])['vout'][int(vout)]['value'])
    with ThreadPoolExecutor(2) as pool:
        list(pool.map(lambda w:bark(w,'refresh','--all'),wallets))
    mine(3)
    coins=[bark(w,'vtxos')[0] for w in wallets]
    assert all(c['id']!=b['id'] for c,b in zip(coins,boards)),'refresh funding not claimed'
    expire(coins)
    for _ in range(4):
        ticks()
        if all(row(c['id']) for c in coins):break
        mine(3)
    assert all(row(c['id']) for c in coins),'abandoned entitlement not settled'
    mine(6)
    proofs=settle(coins)
    fresh=set(q('SELECT DISTINCT txid FROM expiry_settlement').splitlines())-payment_ids
    gross=sum(sum(o['amount_sat']+o['fee_sat'] for o in json.loads((F/'receipts'/(t+'.json')).read_text())['outputs']) for t in fresh)
    rounds=q(f'SELECT funding_txid FROM round WHERE id>{rid}').splitlines()
    anchors=sorted({c['chain_anchor'] for c in boards+coins})
    quoted=','.join("'"+a+"'" for a in anchors)
    sweeps=q(f'SELECT DISTINCT onchain_spent_txid FROM vtxo WHERE vtxo_id IN ({quoted}) AND onchain_spent_txid IS NOT NULL').splitlines()
    swept=set()
    for txid in sweeps:
        tx=rpc('getrawtransaction',[txid,True])
        swept.update(f"{i['txid']}:{i['vout']}" for i in tx['vin'])
        assert all(o['scriptPubKey'].get('address')==(F/'sweep-address.txt').read_text().strip() for o in tx['vout'])
    assert set(anchors)<=swept,'every cycle backing output must actually be swept'
    round_fees=sum(map(chain_fee,rounds))
    sweep_fees=sum(map(chain_fee,sweeps))
    end=rounds_balance()
    expected=start+inflow-gross-round_fees-sweep_fees
    record=dict(cycle=cycle+1,start_sat=start,board_inflow_sat=inflow,payout_gross_sat=gross,
        round_fees_sat=round_fees,sweep_fees_sat=sweep_fees,end_sat=end,expected_sat=expected,
        boards=boards,coins=coins,payments=proofs,round_txids=rounds,sweep_txids=sweeps)
    ledger.append(record);save('liquidity-ledger.json',ledger)
    event('cycle',**{k:v for k,v in record.items() if k not in ['boards','coins','payments','round_txids','sweep_txids']})
    assert end==expected,record
assert len(ledger)==cycles
event('PASS',scenario='capital recycles with no rounds-wallet faucet top-ups',cycles=cycles,
    start_sat=initial,end_sat=ledger[-1]['end_sat'],board_inflow_sat=sum(r['board_inflow_sat'] for r in ledger),
    recipient_gross_sat=sum(r['payout_gross_sat'] for r in ledger),
    operator_mining_fees_sat=sum(r['round_fees_sat']+r['sweep_fees_sat'] for r in ledger))
