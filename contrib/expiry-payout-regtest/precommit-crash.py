from common import *

configure_task(enabled=False)
_,coin=board('precommit-crash',100000)
expire([coin])
q("CREATE FUNCTION expiry_precommit_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(50321); RETURN NEW; END $$; CREATE TRIGGER expiry_precommit_hold BEFORE INSERT ON expiry_settlement FOR EACH ROW EXECUTE FUNCTION expiry_precommit_hold()")
lock=subprocess.Popen(['docker','exec','-i',PROJECT+'-postgres','psql','-X','-U','postgres','-d','expiry_task','-At'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(OUT/'precommit-lock.log').open('w'),text=True)
try:
    lock.stdin.write("SELECT pg_advisory_lock(50321); SELECT 'locked';\n");lock.stdin.flush()
    while lock.stdout.readline().strip()!='locked': assert lock.poll() is None
    configure_task(enabled=True)
    wait(lambda:int(q("SELECT count(*) FROM pg_stat_activity WHERE datname='expiry_task' AND wait_event='advisory' AND query LIKE 'INSERT INTO expiry_settlement%'"))>0,'signed payment waits before atomic commit')
    assert row(coin['id']) is None
    stop_daemon('captaind',kill=True)
    lock.stdin.close();lock.wait(timeout=5)
    q('DROP TRIGGER expiry_precommit_hold ON expiry_settlement; DROP FUNCTION expiry_precommit_hold()')
    assert row(coin['id']) is None
    assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{coin['id']}'")=='spendable'
    start_daemon('captaind')
    settle([coin]);mine(3)
    proof=payment(coin['id'])
    assert q(f"SELECT count(*) FROM expiry_settlement WHERE id='{coin['id']}'")=='1'
    assert rpc('getrawtransaction',[proof['txid'],True])['confirmations']>=3
    save('precommit-crash-proof.json',proof)
    event('PASS',scenario='SIGKILL after signing before atomic commit leaves entitlement payable',**proof)
finally:
    if lock.poll() is None:
        lock.stdin.close();lock.wait(timeout=5)
    q('DROP TRIGGER IF EXISTS expiry_precommit_hold ON expiry_settlement; DROP FUNCTION IF EXISTS expiry_precommit_hold()')
    if not listening(48535):start_daemon('captaind')
