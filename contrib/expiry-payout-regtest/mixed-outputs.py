"""Cancel all duplicate/distinct replacement requests, retaining original value."""
from common import *
import sqlite3,shutil

configure_task(enabled=False)
owner,first=board('mixed-outputs',150000)
bark(owner,'board','150000 sat');mine(4)
inputs=bark(owner,'vtxos');assert len(inputs)==2
backup=owner+'-exit';shutil.copytree(E/'wallets'/owner,E/'wallets'/backup)
with sqlite3.connect(E/'wallets'/owner/'db.sqlite') as db:
    key_indexes=dict(db.execute('SELECT public_key,idx FROM bark_vtxo_key'))
request=dict(mnemonic=(E/'wallets'/owner/'mnemonic').read_text(),inputs=[dict(
    vtxo=q(f"SELECT encode(vtxo,'hex') FROM vtxo WHERE vtxo_id='{c['id']}'"),index=key_indexes[c['user_pubkey']]) for c in inputs],
    outputs=[dict(index=i,amount=99000) for i in [40,41,41]])
# Real attestations bind every requested key/amount, including duplicate requests.
hash=cmd([ROOT/'target/debug/examples/delegated_request'],'submit-exchange',input=json.dumps(request)).stdout.strip()
rid=wait(lambda:q(f"SELECT spent_in_round FROM vtxo WHERE vtxo_id='{first['id']}'"),'exchange round')
rows=json.loads(q(f"SELECT json_agg(x) FROM (SELECT vtxo_id id,amount amount_sat,expiry expiry_height,anchor_point chain_anchor FROM vtxo WHERE spend_state='unclaimed' AND amount=99000 AND anchor_point LIKE (SELECT funding_txid||':%' FROM round WHERE id={rid})) x"))
assert len(rows)==3,rows
for c in rows:
    raw=q(f"SELECT encode(vtxo,'hex') FROM vtxo WHERE vtxo_id='{c['id']}'")
    c.update(json.loads(cmd([ROOT/'target/debug/examples/vtxo_path','--json'],'decode-output',input=raw).stdout))
assert len({c['user_pubkey'] for c in rows})==2,'fixture must include duplicate keys'
config=E/'wallets'/backup/'config.toml';config.write_text(config.read_text().replace(':48535',':48534'))
bark(backup,'exit','start','--vtxo',first['id'])
for _ in range(40):
    if q(f"SELECT confirmed_height IS NOT NULL FROM vtxo WHERE vtxo_id='{first['id']}'")=='t':break
    bark(backup,'exit','progress');mine(1)
else:raise AssertionError('original exit leaf missing')
expire(rows)
remaining=next(c for c in inputs if c['id']!=first['id'])
# A spent replacement must veto the whole cancellation. This is an explicit
# database-state fault; the exchange and all chain transactions remain genuine.
q(f"UPDATE vtxo SET spend_state='spent',updated_at=NOW() WHERE vtxo_id='{rows[0]['id']}'")
try:
    configure_task(enabled=True);ticks(5)
    assert q(f"SELECT count(*) FROM expiry_cancelled_participation WHERE id='{hash}'")=='0'
    assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{remaining['id']}'")=='spent'
finally:
    configure_task(enabled=False)
    q(f"UPDATE vtxo SET spend_state='unclaimed',updated_at=NOW() WHERE vtxo_id='{rows[0]['id']}'")
configure_task(enabled=True)
proof=settle([remaining])[0];mine(3)
assert row(first['id']) is None and all(row(c['id']) is None for c in rows)
assert all(q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{c['id']}'")=='spent' for c in rows)
assert q(f"SELECT count(*) FROM round_participation WHERE unlock_hash='{hash}'")=='0'
audit=json.loads(q(f"SELECT row_to_json(x) FROM expiry_cancelled_participation x WHERE id='{hash}'"))
assert set(audit['output_ids'])=={c['id'] for c in rows}
assert set(audit['input_ids'])=={c['id'] for c in inputs}
assert audit['exited_ids']==[first['id']]
tx=rpc('getrawtransaction',[proof['txid'],True])
paid=[o for o in tx['vout'] if o['scriptPubKey'].get('address')==payout_address(remaining)]
assert len(paid)==1
receipt=next(o for o in proof['receipt']['outputs'] if o['vout']==paid[0]['n'])
assert SAT(paid[0]['value'])+receipt['fee_sat']==remaining['amount_sat']
for c in rows+[first]:assert all(o['scriptPubKey'].get('address')!=payout_address(c) for o in tx['vout'])
ticks(5)
assert row(remaining['id'])['txid']==proof['txid']
save('mixed-outputs-proof.json',dict(owner=owner,inputs=inputs,replacements=rows,audit=audit,payment=proof))
event('PASS',scenario='three replacements, duplicate keys, spent-state veto, atomic original-only settlement',txid=proof['txid'])
