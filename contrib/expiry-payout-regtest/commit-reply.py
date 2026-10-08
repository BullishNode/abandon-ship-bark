"""Drop the real PostgreSQL COMMIT reply after the server committed a payout."""
from common import *

configure_task(enabled=False)
import socketserver,struct
from concurrent.futures import ThreadPoolExecutor

owner,coin=board('commit-reply',110000)
expire([coin])
fired=threading.Event()
errors=[]
def exact(sock,count):
    result=b''
    while len(result)<count:
        block=sock.recv(count-len(result))
        if not block:raise EOFError()
        result+=block
    return result

class Session(socketserver.BaseRequestHandler):
    def handle(self):
        upstream=socket.create_connection(('127.0.0.1',50432))
        seen_receipt=threading.Event()
        def replies():
            try:
                while True:
                    tag=exact(upstream,1);length=exact(upstream,4)
                    body=exact(upstream,struct.unpack('!I',length)[0]-4)
                    # PostgreSQL emits CommandComplete(COMMIT) only after its
                    # transaction has committed. Never forward that reply.
                    if tag==b'C' and body==b'COMMIT\0' and seen_receipt.is_set() and not fired.is_set():
                        fired.set()
                        self.request.shutdown(socket.SHUT_RDWR)
                        upstream.shutdown(socket.SHUT_RDWR)
                        return
                    self.request.sendall(tag+length+body)
                    if tag==b'Z' and body==b'I':seen_receipt.clear()
            except (EOFError,ConnectionError,OSError):pass
        try:
            # No TLS on this isolated PostgreSQL fixture. Startup is the only
            # untagged packet; every subsequent message has tag+length framing.
            length=exact(self.request,4)
            startup=exact(self.request,struct.unpack('!I',length)[0]-4)
            assert startup[:4]==struct.pack('!I',196608),'unexpected PostgreSQL negotiation'
            upstream.sendall(length+startup)
            threading.Thread(target=replies,daemon=True).start()
            while True:
                tag=exact(self.request,1);length=exact(self.request,4)
                body=exact(self.request,struct.unpack('!I',length)[0]-4)
                if b'INSERT INTO expiry_settlement' in body:seen_receipt.set()
                upstream.sendall(tag+length+body)
        except (EOFError,ConnectionError,OSError):pass
        except Exception as error:errors.append(str(error))
        finally:upstream.close()

class Proxy(socketserver.ThreadingTCPServer):
    allow_reuse_address=True
    daemon_threads=True
proxy=Proxy(('127.0.0.1',0),Session)
proxy_port=proxy.server_address[1]
event('postgres-proxy',port=proxy_port)
threading.Thread(target=proxy.serve_forever,daemon=True).start()
config=F/'captaind.toml';original=config.read_text()
stop_daemon('captaind')
try:
    config.write_text(original.replace('50432',str(proxy_port)).replace('enabled = false','enabled = true'))
    start_daemon('captaind')
    proof=settle([coin])[0]
    assert fired.is_set(),('COMMIT reply was not dropped',errors)
    assert not errors,errors
    r=row(coin['id'])
    wait(lambda:r['txid'] in rpc('getrawmempool'),'broadcast after lost database reply')
    ticks()
    assert row(coin['id'])==r
    mine(3)
    proof=payment(coin['id'])
    save('commit-reply-proof.json',proof)
    event('PASS',scenario='lost real COMMIT reply resolves durable receipt before releasing wallet inputs',txid=proof['txid'])
finally:
    if listening(48535):stop_daemon('captaind')
    config.write_text(original.replace('enabled = false','enabled = true'))
    proxy.shutdown()
    start_daemon('captaind')
