"""SIGKILL after the atomic commit; recover the same tx with new payouts disabled."""
from common import *
import http.server, urllib.error

config=F/'captaind.toml'
original=config.read_text()
disabled=original.replace('enabled = true','enabled = false')
assert disabled!=original
lock=None
server=None
try:
    stop_daemon('captaind');config.write_text(disabled);start_daemon('captaind')
    owner,coin=board('native-crash',130000)
    expire([coin])
    assert row(coin['id']) is None
    # The atomic transaction stores derivation metadata BEFORE its receipt.
    # This holds the separate wallet-graph persist AFTER that receipt commits.
    q(f"CREATE FUNCTION expiry_wallet_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind::text='rounds' AND EXISTS (SELECT 1 FROM expiry_settlement WHERE id='{coin['id']}') THEN PERFORM pg_advisory_xact_lock(50319); END IF; RETURN NEW; END $$; CREATE TRIGGER expiry_wallet_hold BEFORE INSERT ON wallet_changeset FOR EACH ROW EXECUTE FUNCTION expiry_wallet_hold()")
    lock=subprocess.Popen(D[:2]+['-i']+D[2:]+['psql','-X','-U','postgres','-d','expiry_task','-At'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(OUT/'wallet-lock.log').open('w'),text=True)
    lock.stdin.write("SELECT pg_advisory_lock(50319); SELECT 'locked';\n");lock.stdin.flush()
    while lock.stdout.readline().strip()!='locked':assert lock.poll() is None

    class Proxy(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body=self.rfile.read(int(self.headers['Content-Length']))
            request=json.loads(body)
            if request['method']=='sendrawtransaction':
                # Never forward an old process's delayed broadcast after it dies.
                self.send_error(503);return
            headers={'Authorization':self.headers['Authorization'],'Content-Type':'application/json'}
            try:
                with urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:53443'+self.path,body,headers),timeout=30) as response:
                    status,data=response.status,response.read()
            except urllib.error.HTTPError as e:status,data=e.code,e.read()
            self.send_response(status);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        def log_message(self,*args):pass
    server=http.server.ThreadingHTTPServer(('127.0.0.1',53444),Proxy)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    stop_daemon('captaind');config.write_text(original.replace(':53443',':53444'));start_daemon('captaind')
    before=wait(lambda:row(coin['id']),'atomic native receipt')
    wait(lambda:int(q("SELECT count(*) FROM pg_stat_activity WHERE datname='expiry_task' AND wait_event='advisory' AND query LIKE '%INSERT INTO wallet_changeset%'"))>0,'wallet persist held after atomic commit')
    assert before['txid'] not in rpc('getrawmempool')
    assert q(f"SELECT encode(tx,'hex') FROM nursery_tx WHERE txid='{before['txid']}'")==before['raw']
    stop_daemon('captaind',kill=True)
    lock.stdin.close();lock.wait(timeout=5)
    q('DROP TRIGGER expiry_wallet_hold ON wallet_changeset; DROP FUNCTION expiry_wallet_hold()')
    config.write_text(disabled);start_daemon('captaind')
    wait(lambda:before['txid'] in rpc('getrawmempool'),'startup nursery broadcasts with task disabled')
    assert rpc('getrawtransaction',[before['txid']])==before['raw']
    assert row(coin['id'])==before
    assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{coin['id']}'")=='spent'
    mine(3)
    stop_daemon('captaind');config.write_text(original);start_daemon('captaind')
    wait(lambda:(F/'receipts'/(before['txid']+'.json')).exists(),'receipt regeneration after reenable')
    proof=payment(coin['id'])
    assert proof['txid']==before['txid']
    assert q(f"SELECT count(*) FROM expiry_settlement WHERE id='{coin['id']}'")=='1'
    save('crash-proof.json',dict(wallet=owner,coin=coin,proof=proof,raw_preserved=True,disabled_restart=True))
    event('PASS',scenario='SIGKILL after atomic commit; disabled startup broadcasts same bytes; receipt recovers',**proof)
finally:
    if lock is not None and lock.poll() is None:
        lock.stdin.close();lock.wait(timeout=5)
    q('DROP TRIGGER IF EXISTS expiry_wallet_hold ON wallet_changeset; DROP FUNCTION IF EXISTS expiry_wallet_hold()')
    if listening(48535):stop_daemon('captaind')
    config.write_text(original)
    if server is not None:server.shutdown();server.server_close()
    start_daemon('captaind')
