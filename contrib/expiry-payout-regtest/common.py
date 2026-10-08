"""Private approach-D fixture. SQL observes state and injects documented faults.

All funds are regtest, every daemon/volume/port belongs to abandon-captaind-task.
No automatic funding after bootstrap; no volume deletion.
"""
import base64, json, os, signal, socket, subprocess, sys, threading, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
E = Path(os.environ.get('EXPIRY_EVIDENCE', '/home/francis/bark-integration-2026-10-01/expiry-task-evidence/runtime'))
F = E / 'fixture'
B = ROOT
OUT = E / os.environ.get('EXPIRY_CASE', 'native-tests')
OUT.mkdir(parents=True,exist_ok=True)
PROJECT = 'abandon-captaind-task'
D = ['docker', 'exec', PROJECT + '-postgres']
IMAGE = 'abandon-ship/bark:release-3a8f9694a'
NATIVE_IMAGE = os.environ.get('EXPIRY_NATIVE_IMAGE')
N = 0
COMMAND_LOCK = threading.Lock()
SAT = lambda btc: round(btc * 100000000)


def save(name, obj):
    (OUT / name).write_text(json.dumps(obj, indent=2) + '\n')
    return obj


def event(name, **data):
    print(json.dumps(dict(event=name, **data)), flush=True)


def cmd(args, label, *, input=None, check=True, timeout=300):
    global N
    with COMMAND_LOCK:
        N += 1
        sequence = N
    args=list(map(str,args))
    p = subprocess.run(args, input=input, text=True, capture_output=True, timeout=timeout)
    (OUT / f'{sequence:04}-{label}.log').write_text(p.stdout + p.stderr)
    if check:
        assert p.returncode == 0, (label,p.returncode,p.stderr[-2000:])
    return p


def q(sql):
    return subprocess.check_output(D + ['psql','-X','-U','postgres','-d','expiry_task',
        '-v','ON_ERROR_STOP=1','-Atc',sql], text=True).strip()


def rpc(method, params=None, wallet='faucet', port=53443):
    req = urllib.request.Request(f'http://127.0.0.1:{port}/wallet/{wallet}',
        json.dumps(dict(jsonrpc='2.0',id=1,method=method,params=params or [])).encode(),
        {'Authorization':'Basic '+base64.b64encode(b'second:ark').decode(), 'Content-Type':'application/json'})
    result = json.load(urllib.request.urlopen(req,timeout=1200 if method=='scantxoutset' else 120))
    if result.get('error'): raise RuntimeError((method,result['error']))
    return result['result']


def wait(predicate, label, timeout=180):
    deadline = time.monotonic()+timeout
    while time.monotonic()<deadline:
        result = predicate()
        if result: return result
        time.sleep(.2)
    raise AssertionError('timeout: '+label)


def listening(port):
    with socket.socket() as s: return s.connect_ex(('127.0.0.1',port)) == 0


def start_daemon(name, image=None):
    port = 48535 if name=='captaind' else 48538
    assert not listening(port)
    args=[str(B/'target/debug'/name),'--config',str(F/(name+'.toml')),'start']
    daemon_image=image or NATIVE_IMAGE
    if daemon_image:
        container=PROJECT+'-'+name
        args=['docker','run','--rm','--name',container,'--network','host','--user','1000:1000',
            '-v',str(E)+':'+str(E),'-e','RUST_LOG=info','-e','CAPTAIND_LOG=info',
            '-e','WATCHMAND_LOG=info','--entrypoint',name,daemon_image,*args[1:]]
        (F/(name+'.container')).write_text(container)
    p = subprocess.Popen(args,
        stdout=(OUT/(name+'.log')).open('a'),stderr=subprocess.STDOUT,
        env={**os.environ,'RUST_LOG':'info','CAPTAIND_LOG':'info','WATCHMAND_LOG':'info'})
    (F/(name+'.pid')).write_text(str(p.pid))
    def ready():
        assert p.poll() is None, name+' startup failed'
        return listening(port)
    wait(ready,name+' start')


def stop_daemon(name, kill=False):
    marker=F/(name+'.container')
    if marker.exists():
        container=marker.read_text()
        cmd(['docker','kill' if kill else 'stop',container],'stop-'+name)
        marker.unlink()
        wait(lambda: not listening(48535 if name=='captaind' else 48538),name+' stop')
        wait(lambda:subprocess.run(['docker','inspect',container],stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL).returncode!=0,name+' container removal')
        return
    pid = int((F/(name+'.pid')).read_text())
    executable = Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')[0].decode()
    assert executable == str(B/'target/debug'/name), executable
    os.kill(pid,signal.SIGKILL if kill else signal.SIGTERM)
    wait(lambda: not listening(48535 if name=='captaind' else 48538),name+' stop')


def synced():
    tip = rpc('getblockcount')
    previous = -1
    deadline = time.monotonic()+300
    while time.monotonic()<deadline:
        heights = q('SELECT least((SELECT max(height) FROM captaind_block),(SELECT max(height) FROM watchmand_block))')
        height = int(heights or -1)
        if height >= tip: return
        if height > previous: previous,deadline = height,time.monotonic()+300
        time.sleep(.2)
    raise AssertionError(f'daemon tips stalled {previous}/{tip}')


def mine(n):
    if n>0: rpc('generatetoaddress',[n,rpc('getnewaddress')])
    synced()


def bark_args(wallet,*args):
    return ['docker','run','--rm','--network','host','--user','1000:1000',
        '-v',str(E/'wallets')+':/wallets','--entrypoint','bark',IMAGE,
        '--datadir','/wallets/'+wallet,*args]


def bark(wallet,*args,check=True):
    p=cmd(bark_args(wallet,*args),'bark-'+args[0],check=check)
    if not check: return p
    return json.loads(p.stdout) if p.stdout.strip() else None


def newwallet(label, funding_sat=2000000):
    name = label+'-'+str(time.time_ns())
    bark(name,'create','--regtest','--ark','http://127.0.0.1:48535',
        '--bitcoind','http://127.0.0.1:53443','--bitcoind-user','second','--bitcoind-pass','ark')
    if funding_sat:
        rpc('sendtoaddress',[bark(name,'onchain','address')['address'],funding_sat/1e8,'','',False,True,None,'unset',None,3])
    return name


def board(label, amount=60000):
    w = newwallet(label)
    mine(3)
    bark(w,'board',str(amount)+' sat')
    mine(3)
    return w,bark(w,'vtxos')[0]


def prime():
    if 'feerate' in rpc('estimatesmartfee',[6]): return
    for _ in range(12):
        for _ in range(8):
            rpc('sendtoaddress',[rpc('getnewaddress'),.001,'','',False,True,None,'unset',None,3])
        mine(1)
    assert 'feerate' in rpc('estimatesmartfee',[6])


def expire(coins):
    mine(max(0,max(c['expiry_height'] for c in coins)+145-rpc('getblockcount')))
    for _ in range(90):
        if all(q(f"SELECT onchain_spent_txid IS NOT NULL FROM vtxo WHERE vtxo_id='{c['chain_anchor']}'")=='t' for c in coins): break
        time.sleep(.5)
        mine(1)
    else: raise AssertionError('watchman sweep not observed')
    mine(6)
    prime()


def row(id):
    data=q(f"SELECT row_to_json(s) FROM (SELECT s.id,s.txid,s.fee_sat,encode(s.raw_tx,'hex') raw FROM expiry_settlement s WHERE s.id='{id}') s")
    return json.loads(data) if data else None


def payment(id):
    r=row(id);assert r,id
    assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{id}'")=='spent'
    tx=rpc('getrawtransaction',[r['txid'],True])
    fee=sum(SAT(rpc('getrawtransaction',[i['txid'],True])['vout'][i['vout']]['value']) for i in tx['vin'])-sum(SAT(o['value']) for o in tx['vout'])
    receipt=json.loads((F/'receipts'/(r['txid']+'.json')).read_text())
    assert receipt['txid']==r['txid']
    assert sum(o['fee_sat'] for o in receipt['outputs'])==fee==r['fee_sat']>0
    for out in receipt['outputs']:
        assert SAT(tx['vout'][out['vout']]['value'])==out['amount_sat']
        assert out['fee_sat']*100 <= (out['amount_sat']+out['fee_sat'])*20
    assert q(f"SELECT count(*) FROM nursery_tx WHERE txid='{r['txid']}' AND kind::text='expiry-payout'")=='1'
    return dict(id=id,txid=r['txid'],fee_sat=fee,receipt=receipt)


def payout_address(coin):
    d=rpc('getdescriptorinfo',['tr('+coin['user_pubkey'][2:]+')'])['descriptor']
    return rpc('deriveaddresses',[d])[0]
