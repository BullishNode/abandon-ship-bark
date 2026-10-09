from common import *

configure_task(enabled=False)
_,coin=board('receipt-outage',80000)
expire([coin])
blocked=OUT/'blocked-receipts';blocked.write_text('Intentional directory fault.\n')
try:
    configure_task(enabled=True,receipt_dir=str(blocked))
    r=wait(lambda:row(coin['id']),'committed payout despite receipt outage')
    wait(lambda:r['txid'] in rpc('getrawmempool'),'nursery broadcast despite receipt outage')
    mine(3)
    assert rpc('getrawtransaction',[r['txid'],True])['confirmations']>=3
    assert not (F/'receipts'/(r['txid']+'.json')).exists()
    ticks()
    assert 'expiry receipt' in (OUT/'captaind.log').read_text()
finally:configure_task(enabled=True,receipt_dir=str(F/'receipts'))
proof=settle([coin])[0]
file=F/'receipts'/(proof['txid']+'.json');old=file.read_bytes();file.unlink()
ticks()
assert file.read_bytes()==old
assert q(f"SELECT count(*) FROM expiry_settlement WHERE id='{coin['id']}'")=='1'
save('receipt-outage-proof.json',proof)
event('PASS',scenario='receipt publication failure cannot lose payout; exact metadata regenerates without another payment')
