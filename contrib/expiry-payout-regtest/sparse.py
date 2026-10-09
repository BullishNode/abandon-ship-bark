from common import *

configure_task(enabled=True)
import re,sqlite3
from concurrent.futures import ThreadPoolExecutor

owner=newwallet('sparse')
mine(3)
seed=(E/'wallets'/owner/'mnemonic').read_text()
def derive(index=None):
    args=[str(ROOT/'target/debug/examples/coin_key_descriptor'),'regtest']
    if index is not None:args.append(str(index))
    return subprocess.run(args,input=seed,text=True,capture_output=True,check=True).stdout.strip()

descriptor=derive()
coins=[]
indexes=[0,257,65537,999999]
for index in indexes:
    # Store genuine seed-derived history, with no fabricated coin entitlement.
    if index:
        with sqlite3.connect(E/'wallets'/owner/'db.sqlite') as db:
            db.execute('INSERT INTO bark_vtxo_key(idx,public_key) VALUES(?,?)',(index-1,derive(index-1)))
    bark(owner,'board','30000 sat')
    mine(4)
    coin=next(c for c in bark(owner,'vtxos') if c['user_pubkey']==derive(index))
    coins.append(coin)
expire(coins)
settle(coins);mine(3)
proofs=[payment(c['id']) for c in coins]
descriptor += '#'+rpc('getdescriptorinfo',[descriptor])['checksum']
narrow=rpc('scantxoutset',['start',[dict(desc=descriptor,range=[0,200])]])
assert len(narrow['unspents'])==1,narrow
core_pid=int(subprocess.check_output(['docker','inspect','-f','{{.State.Pid}}',PROJECT+'-bitcoind'],text=True))
def rss():
    s=Path(f'/proc/{core_pid}/status').read_text()
    return int(re.search(r'^VmRSS:\s+(\d+)',s,re.M).group(1))
baseline=rss();samples=[];start=time.monotonic()
with ThreadPoolExecutor(1) as pool:
    future=pool.submit(rpc,'scantxoutset',['start',[dict(desc=descriptor,range=[0,999999])]])
    while not future.done():samples.append(rss());time.sleep(.1)
    scan=future.result()
elapsed=time.monotonic()-start
found=scan['unspents']
assert len(found)==4 and {o['txid'] for o in found}=={p['txid'] for p in proofs},found
discovered=sorted({int(re.search(r'/(\d+)\]',o['desc']).group(1)) for o in found})
assert discovered==indexes,discovered
recover='sparse-seed-'+str(time.time_ns())
rpc('createwallet',[recover,False,True])
imports=[]
for index in discovered:
    fixed=descriptor.split('#')[0].replace('*',str(index))
    fixed+='#'+rpc('getdescriptorinfo',[fixed])['checksum']
    imports.append(dict(desc=fixed,timestamp=0))
assert all(r['success'] for r in rpc('importdescriptors',[imports],wallet=recover))
rate=rpc('estimatesmartfee',[6])['feerate']*1e5
spend=rpc('sendall',dict(recipients=[rpc('getnewaddress')],fee_rate=round(rate,3)),wallet=recover)['txid']
mine(1)
raw=rpc('getrawtransaction',[spend,True])
assert raw['confirmations']>=1
assert {(i['txid'],i['vout']) for i in raw['vin']}=={(o['txid'],o['vout']) for o in found}
measurement=dict(scan_range=[0,999999],indexes=discovered,seconds=elapsed,core_rss_before_kib=baseline,
    core_peak_rss_kib=max(samples),core_version=rpc('getnetworkinfo')['subversion'],
    matched_outputs=found,recovery_spend=spend,proofs=proofs)
save('sparse-proof.json',measurement)
event('PASS',scenario='real native expiry payouts at sparse indexes; seed-only Core finds and spends',
    range=[0,999999],indexes=discovered,seconds=elapsed,core_rss_before_kib=baseline,core_peak_rss_kib=max(samples))
