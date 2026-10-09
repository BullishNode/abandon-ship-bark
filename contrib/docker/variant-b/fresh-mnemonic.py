"""Regression for browser-generated seeds against a real Core node.

Requires Docker, an available regtest Ark server (ARK_SERVER), and the previous
release image. Set NEW_BARK_IMAGE to test the exact new image, or build barkd.
Leaves its isolated containers/data for inspection; does not alter Ark funds.
"""
from pathlib import Path
import subprocess as sp,urllib.request,urllib.error,json,time,socket,base64,os,tempfile
ROOT=Path(__file__).resolve().parents[3]
E=Path(tempfile.mkdtemp(prefix='bark-fresh-mnemonic-'));E.chmod(0o700)
port=13501; api=13502
for p in [port,api]:
 with socket.socket() as s: assert s.connect_ex(('127.0.0.1',p))!=0,p
core='abandon-fresh-mnemonic-'+str(time.time_ns())
image='bitcoin/bitcoin@sha256:a1d05be6939b5c85132a761d2b795f5b8a2612ce53782263da4939778e461792'
def cmd(*a):return sp.check_output(list(map(str,a)),text=True).strip()
cmd('docker','run','-d','--name',core,'--network','host',image,'bitcoind','-regtest','-server','-listen=0','-rpcbind=127.0.0.1','-rpcallowip=127.0.0.1','-rpcport='+str(port),'-rpcuser=second','-rpcpassword=ark','-txindex')
def rpc(method,params=[]):
 r=urllib.request.Request('http://127.0.0.1:'+str(port),json.dumps(dict(jsonrpc='2.0',id=1,method=method,params=params)).encode(),{'Authorization':'Basic '+base64.b64encode(b'second:ark').decode(),'Content-Type':'application/json'})
 d=json.load(urllib.request.urlopen(r,timeout=60));assert not d.get('error'),d;return d['result']
def wait(f):
 for _ in range(180):
  try:return f()
  except Exception:time.sleep(.2)
 raise RuntimeError('wait timeout')
wait(lambda:rpc('getblockcount'));rpc('createwallet',['faucet']);rpc('generatetoaddress',[101,rpc('getnewaddress')])
request={'network':'regtest','ark_server':os.environ.get('ARK_SERVER','http://127.0.0.1:48535'),'chain_source':{'bitcoind':{'bitcoind':'http://127.0.0.1:'+str(port),'bitcoind_auth':{'user-pass':{'user':'second','pass':'ark'}}}},'mnemonic':'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about'}
def call(path,data=None):
 r=urllib.request.Request('http://127.0.0.1:'+str(api)+'/api/v1/'+path,json.dumps(data).encode() if data is not None else None,{'Content-Type':'application/json'})
 try:
  with urllib.request.urlopen(r,timeout=90) as x:return x.status,json.load(x)
 except urllib.error.HTTPError as e:return e.code,e.read().decode()
results=[]
for mode in ['before','after']:
 directory=E/mode;directory.mkdir(mode=0o700)
 args=['--datadir',str(directory),'--host','127.0.0.1','--port',str(api),'--no-auth']
 if mode=='before' or os.environ.get('NEW_BARK_IMAGE'):
  name=core+'-'+mode;cmd('docker','run','-d','--name',name,'--network','host','--user','1000:1000','-v',str(directory)+':'+str(directory),'--entrypoint','barkd',('abandon-ship/bark:release-3a8f9694a' if mode=='before' else os.environ['NEW_BARK_IMAGE']),*args)
 else:
  log=(E/'after.log').open('w');proc=sp.Popen([str(ROOT/'target/debug/barkd'),*args],stdout=log,stderr=sp.STDOUT)
 def ready():
  with socket.create_connection(('127.0.0.1',api),timeout=1):return True
 wait(ready)
 try:
  if mode=='after':
   status,body=call('wallet/create',request)
   assert status==500 and 'birthday-height' in body,(status,body)
   results.append({'case':'existing seed without birthday still rejected','status':status})
  status,body=call('wallet/create',request|{'fresh_mnemonic':True})
  results.append({'case':mode,'status':status,'response':body})
  assert status==(500 if mode=='before' else 200),(status,body)
  if mode=='before':assert 'birthday-height' in body
  else:
   status,body=call('onchain/addresses/next',{});assert status==200,(status,body)
   results.append({'case':'fresh wallet usable','address':body['address']})
 finally:
  if mode=='before' or os.environ.get('NEW_BARK_IMAGE'):cmd('docker','stop',name)
  else:proc.terminate();proc.wait(timeout=60);log.close()
cmd('docker','stop',core)
(E/'result.json').write_text(json.dumps({'status':'PASS','results':results,'core_container':core,'height':101},indent=2)+'\n')
print(json.dumps({'status':'PASS','evidence':str(E),'results':results}))
