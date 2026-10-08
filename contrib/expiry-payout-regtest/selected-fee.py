"""A real selected-target estimate remains usable if a different target fails."""
from common import *
import http.server,urllib.error

configure_task(enabled=False)
owner,coin=board('selected-fee',60000)
expire([coin])
assert 'feerate' in rpc('estimatesmartfee',[6,'economical'])
original=(F/'captaind.toml').read_text()
calls=[]
class EstimateProxy(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body=self.rfile.read(int(self.headers['Content-Length']));request=json.loads(body)
        if request['method']=='estimatesmartfee':calls.append(request['params'][0])
        if request['method']=='estimatesmartfee' and request['params'][0]==1:
            data=json.dumps(dict(jsonrpc='2.0',id=request['id'],result=dict(blocks=1,errors=['test: only fast target unavailable']),error=None)).encode()
            status=200
        else:
            headers={'Authorization':self.headers['Authorization'],'Content-Type':'application/json'}
            try:
                with urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:53443'+self.path,body,headers),timeout=30) as response:
                    status,data=response.status,response.read()
            except urllib.error.HTTPError as e:status,data=e.code,e.read()
        self.send_response(status);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),EstimateProxy)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    stop_daemon('captaind')
    enabled=original.replace('enabled = false','enabled = true').replace(':53443',':'+str(server.server_address[1]))
    (F/'captaind.toml').write_text(enabled);start_daemon('captaind')
    wait(lambda:row(coin['id']),'configured target has a real fee but payout is blocked by another target',20)
    proof=settle([coin])[0]
    assert 6 in calls,'selected target was not queried'
    tx=rpc('getrawtransaction',[proof['txid'],True])
    out=[o for o in tx['vout'] if o['scriptPubKey'].get('address')==payout_address(coin)]
    assert len(out)==1 and SAT(out[0]['value'])+proof['fee_sat']==coin['amount_sat']
    save('selected-fee-proof.json',dict(coin=coin,proof=proof,estimate_targets=calls))
    event('PASS',scenario='selected real six-block estimate pays despite unavailable fast estimate',txid=proof['txid'])
finally:
    save('estimate-targets.json',calls)
    stop_daemon('captaind');(F/'captaind.toml').write_text(original.replace('enabled = false','enabled = true'));start_daemon('captaind')
    server.shutdown();server.server_close()
