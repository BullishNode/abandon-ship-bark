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
if os.environ.get('EXPIRY_CANCEL_FORFEIT')=='1':
    q("CREATE FUNCTION cancel_forfeit_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(50327); RETURN NEW; END $$; CREATE TRIGGER cancel_forfeit_hold BEFORE INSERT ON expiry_cancelled_participation FOR EACH ROW EXECUTE FUNCTION cancel_forfeit_hold()")
    lock=subprocess.Popen(D[:2]+['-i']+D[2:]+['psql','-X','-U','postgres','-d','expiry_task','-At'],
        stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(OUT/'cancel-forfeit-lock.log').open('w'),text=True)
    forfeit=None
    try:
        lock.stdin.write("SELECT pg_advisory_lock(50327); SELECT 'locked';\n");lock.stdin.flush()
        while lock.stdout.readline().strip()!='locked':assert lock.poll() is None
        configure_task(enabled=True)
        wait(lambda:int(q("SELECT count(*) FROM pg_stat_activity WHERE wait_event='advisory' AND query LIKE 'INSERT INTO expiry_cancelled_participation%'"))>0,'cancellation holds input associations')
        forfeit=subprocess.Popen([ROOT/'target/debug/examples/delegated_request'],stdin=subprocess.PIPE,
            stdout=(OUT/'cached-forfeit.stdout').open('w'),stderr=(OUT/'cached-forfeit.stderr').open('w'),text=True)
        forfeit.stdin.write(json.dumps(request|{'forfeit_unlock_hash':hash}));forfeit.stdin.close()
        wait(lambda:int(q("SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE 'UPDATE round_part_input SET signed_forfeit_tx%'"))>0,'verified forfeit has cached participation and waits on its input association')
        lock.stdin.close();lock.wait(timeout=5)
        assert forfeit.wait(timeout=30)!=0,'cached forfeit unexpectedly returned an unlock preimage'
        assert 'inputs not fully matched' in (OUT/'cached-forfeit.stderr').read_text()
        save('cached-forfeit-proof.json',dict(cancellation_won=True,forfeit_rejected=True,unlock_hash=hash))
    finally:
        if lock.poll() is None:lock.stdin.close();lock.wait(timeout=5)
        if forfeit is not None and forfeit.poll() is None:forfeit.terminate();forfeit.wait(timeout=10)
        q('DROP TRIGGER IF EXISTS cancel_forfeit_hold ON expiry_cancelled_participation; DROP FUNCTION IF EXISTS cancel_forfeit_hold()')
if os.environ.get('EXPIRY_CANCEL_CRASH')=='1':
    for table,key,cancelled in [('expiry_cancelled_participation',50325,False),('expiry_settlement',50326,True)]:
        q(f"CREATE FUNCTION cancel_crash_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({key}); RETURN NEW; END $$; CREATE TRIGGER cancel_crash_hold BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION cancel_crash_hold()")
        lock=subprocess.Popen(D[:2]+['-i']+D[2:]+['psql','-X','-U','postgres','-d','expiry_task','-At'],
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(OUT/('crash-lock-'+table+'.log')).open('w'),text=True)
        try:
            lock.stdin.write(f"SELECT pg_advisory_lock({key}); SELECT 'locked';\n");lock.stdin.flush()
            while lock.stdout.readline().strip()!='locked':assert lock.poll() is None
            configure_task(enabled=True)
            wait(lambda:int(q(f"SELECT count(*) FROM pg_stat_activity WHERE datname='expiry_task' AND wait_event='advisory' AND query LIKE 'INSERT INTO {table}%'"))>0,'held '+table)
            stop_daemon('captaind',kill=True)
            lock.stdin.close();lock.wait(timeout=5)
            assert q(f"SELECT count(*) FROM expiry_cancelled_participation WHERE id='{hash}'")==str(int(cancelled))
            assert row(remaining['id']) is None
            assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{remaining['id']}'")==('spendable' if cancelled else 'spent')
            assert all(q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{c['id']}'")==('spent' if cancelled else 'unclaimed') for c in rows)
            save('crash-'+table+'.json',dict(cancel_committed=cancelled,payment_committed=False,remaining=remaining['id']))
        finally:
            if lock.poll() is None:lock.stdin.close();lock.wait(timeout=5)
            q(f'DROP TRIGGER IF EXISTS cancel_crash_hold ON {table}; DROP FUNCTION IF EXISTS cancel_crash_hold()')
            if not listening(48535):
                config=F/'captaind.toml';config.write_text(config.read_text().replace('enabled = true','enabled = false'))
                start_daemon('captaind')
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
