from common import *
from concurrent.futures import ThreadPoolExecutor
import shutil

configure_task(enabled=False)

owner,coin=board('race',110000)
# Separate restored wallet state avoids the offboard attempt reserving the
# input locally before the refresh reaches captaind.
refresher=owner+'-refresh'
shutil.copytree(E/'wallets'/owner,E/'wallets'/refresher)
expire([coin])
q("CREATE FUNCTION expiry_test_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(50318); RETURN NEW; END $$; CREATE TRIGGER expiry_test_hold BEFORE INSERT ON expiry_settlement FOR EACH ROW EXECUTE FUNCTION expiry_test_hold()")
lock=subprocess.Popen(['docker','exec','-i',PROJECT+'-postgres','psql','-X','-U','postgres','-d','expiry_task','-At'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(OUT/'sql-lock.log').open('w'),text=True)
try:
    lock.stdin.write("SELECT pg_advisory_lock(50318); SELECT 'locked';\n");lock.stdin.flush()
    while lock.stdout.readline().strip()!='locked': assert lock.poll() is None
    configure_task(enabled=True)
    wait(lambda:int(q("SELECT count(*) FROM pg_stat_activity WHERE datname='expiry_task' AND wait_event='advisory' AND query LIKE 'INSERT INTO expiry_settlement%'"))>0,'receipt insert waiting on test lock')
    assert row(coin['id']) is None
    offboard=bark(owner,'offboard','--vtxo',coin['id'],'--no-sync',check=False)
    assert offboard.returncode!=0 and ('locked' in offboard.stderr or 'flux' in offboard.stderr),offboard.stderr
    with ThreadPoolExecutor(1) as pool:
        # Registration first uploads its VTXO data; that update may wait on
        # the payout transaction before the later in-flux/state check runs.
        request=pool.submit(bark,refresher,'refresh','--delegated','--all',check=False)
        wait(lambda:request.done() or int(q("SELECT count(*) FROM pg_stat_activity WHERE datname='expiry_task' AND wait_event='transactionid'"))>0,'refresh reaches contested input')
        lock.stdin.write("SELECT pg_advisory_unlock(50318);\n");lock.stdin.close();lock.wait(timeout=5)
        refresh=request.result(timeout=30)
        assert refresh.returncode!=0,refresh.stdout
        assert coin['id'] in refresh.stderr,refresh.stderr
        assert any(s in refresh.stderr.lower() for s in ['locked','flux','spent','unusable']),refresh.stderr
    assert q(f"SELECT count(*) FROM round_part_input WHERE vtxo_id='{coin['id']}'")=='0'

    wait(lambda:row(coin['id']),'native task payment committed')
    wait(lambda:row(coin['id'])['txid'] in rpc('getrawmempool'),'native task nursery broadcast')
    proof=settle([coin])[0]
    assert q(f"SELECT count(*) FROM expiry_settlement WHERE id='{coin['id']}'")=='1'
    save('race-proof.json',proof)
    event('PASS',scenario='native task holds coin lock through commit; offboard and delegated refresh lose',**proof)
finally:
    if lock.poll() is None:
        if not lock.stdin.closed: lock.stdin.close()
        lock.wait(timeout=5)
    q('DROP TRIGGER expiry_test_hold ON expiry_settlement; DROP FUNCTION expiry_test_hold()')
