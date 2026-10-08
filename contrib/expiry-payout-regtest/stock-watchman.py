"""Unmodified watchmand shares the optional payout schema and still sweeps."""
from common import *

stock='sha256:9414f43b7de5ce4a806380d6ece719f73c8c1167ad2f40ccbd0ba6e30789c292'
stop_daemon('watchmand')
try:
    start_daemon('watchmand',image=stock)
    owner,coin=board('stock-watchman',70000)
    expire([coin])
    proof=settle([coin])[0]
    mine(3)
    save('stock-watchman-proof.json',dict(image=stock,coin=coin,payment=proof))
    event('PASS',scenario='unmodified watchmand shares native schema and sweeps',image=stock,txid=proof['txid'])
finally:
    stop_daemon('watchmand')
    start_daemon('watchmand')
