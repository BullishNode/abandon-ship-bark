"""Qualify test timing for a simple signet rehearsal, not deep exit trees."""
from common import *
import re

config=F/'captaind.toml'
original=config.read_text()
short=original
for key,value in [('vtxo_lifetime',4),('vtxo_exit_delta',1),('required_board_confirmations',1),
                  ('grace_blocks',0),('sweep_min_confs',1)]:
    short,count=re.subn(r'(?m)^'+key+r' = .*$',key+' = '+str(value),short)
    assert count==(2 if key=='vtxo_lifetime' else 1),(key,count)
stop_daemon('captaind')
try:
    config.write_text(short.replace('enabled = false','enabled = true'))
    start_daemon('captaind')
    prime()
    owner=newwallet('short-cycle',25000)
    wallet_config=E/'wallets'/owner/'config.toml'
    text=re.sub(r'(?m)^vtxo_(exit_margin|refresh_expiry_threshold) = .*\n','',wallet_config.read_text())
    wallet_config.write_text('vtxo_exit_margin = 1\nvtxo_refresh_expiry_threshold = 1\n'+text)
    mine(1)
    start=rpc('getblockcount')
    bark(owner,'board','20000 sat')
    mine(1)
    board_coin=bark(owner,'vtxos')[0]
    assert board_coin['expiry_height']>rpc('getblockcount')
    bark(owner,'refresh','--all')
    mine(1)
    coin=bark(owner,'vtxos')[0]
    assert coin['id']!=board_coin['id']
    assert row(coin['id']) is None
    mine(max(0,coin['expiry_height']+1-rpc('getblockcount')))
    for _ in range(6):
        if q(f"SELECT onchain_spent_txid IS NOT NULL FROM vtxo WHERE vtxo_id='{coin['chain_anchor']}'")=='t':break
        mine(1);time.sleep(.5)
    else:raise AssertionError('short-cycle sweep not seen')
    mine(1)
    proof=settle([coin])[0]
    mine(1)
    result=dict(owner=owner,start_height=start,payout_height=rpc('getblockcount'),
        lifetime=4,exit_delta=1,grace=0,sweep_depth=1,board=board_coin,coin=coin,payment=proof)
    save('short-cycle-proof.json',result)
    event('PASS',scenario='four-block lifetime board/refresh/expiry/native payout',
        start=start,end=result['payout_height'],txid=proof['txid'])
finally:
    stop_daemon('captaind')
    config.write_text(original)
    start_daemon('captaind')
