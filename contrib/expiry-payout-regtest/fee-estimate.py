"""A missing real estimate must preserve an eligible coin until estimation returns."""
from common import *
import http.server

class NoEstimate(http.server.BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def do_POST(self):
        body=self.rfile.read(int(self.headers['Content-Length']))
        request=json.loads(body)
        if request['method']=='estimatesmartfee':
            result=dict(jsonrpc='2.0',id=request['id'],result={'errors':['test: estimate unavailable']},error=None)
            payload=json.dumps(result).encode()
        else:
            headers={k:v for k,v in self.headers.items() if k.lower() not in ('host','content-length')}
            upstream=urllib.request.Request('http://127.0.0.1:53443'+self.path,body,headers)
            try:
                with urllib.request.urlopen(upstream,timeout=30) as response:payload=response.read()
            except urllib.error.HTTPError as error:payload=error.read()
        self.send_response(200)
        self.send_header('Content-Length',str(len(payload)))
        self.end_headers();self.wfile.write(payload)
    def log_message(self,*args):pass

original=(F/'captaind.toml').read_text()
server=http.server.ThreadingHTTPServer(('127.0.0.1',53445),NoEstimate)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    stop_daemon('captaind')
    (F/'captaind.toml').write_text(original.replace(':53443',':53445'))
    start_daemon('captaind')
    owner,coin=board('native-no-estimate',60000)
    expire([coin])
    time.sleep(3)
    assert row(coin['id']) is None, 'paid using fallback estimate'
    assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{coin['id']}'")=='spendable'
    assert 'no real fee estimate' in (OUT/'captaind.log').read_text()
    save('waiting.json',dict(wallet=owner,coin=coin,tip=rpc('getblockcount'),status='unpaid, spendable, swept, no real estimate'))
finally:
    stop_daemon('captaind')
    (F/'captaind.toml').write_text(original)
    start_daemon('captaind')
    server.shutdown()
wait(lambda:row(coin['id']), 'payment after estimate returns')
wait(lambda:(F/'receipts'/(row(coin['id'])['txid']+'.json')).exists(), 'fee receipt')
proof=payment(coin['id'])
tx=rpc('getrawtransaction',[proof['txid'],True])
recipient=[o for o in tx['vout'] if o['scriptPubKey'].get('address')==payout_address(coin)]
assert len(recipient)==1 and SAT(recipient[0]['value'])+proof['fee_sat']==coin['amount_sat']
mine(3)
save('fee-estimate-proof.json',dict(wallet=owner,coin=coin,proof=proof))
event('PASS',scenario='real estimate outage defers eligible native payment; restoration pays',**proof)
