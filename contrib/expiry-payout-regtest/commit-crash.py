"""Restart while PostgreSQL is still completing the old process's COMMIT."""
from common import *
from concurrent.futures import ThreadPoolExecutor

configure_task(enabled=False)
_,coin=board('commit-crash-payout',50000)
offboarder,other=board('commit-crash-offboard',90000)
expire([coin,other])
# Give both operations one real funding input. With excess UTXOs they can
# select different coins, which does not exercise the restart hazard.
address=(F/'sweep-address.txt').read_text().strip()
destination=rpc('getnewaddress')
stop_daemon('captaind')
drained=cmd([B/'target/debug/captaind','--config',F/'captaind.toml','drain',destination],'withdraw-test-capital')
drain=drained.stdout.strip().splitlines()[-1]
capital=sum(SAT(o['value']) for o in rpc('getrawtransaction',[drain,True])['vout']
    if o['scriptPubKey'].get('address')==destination)
assert capital>1000000
rpc('generatetoaddress',[3,rpc('getnewaddress')]);start_daemon('captaind');synced()
funding=rpc('sendtoaddress',[address,.01,'','',False,True,None,'unset',None,3]);mine(3)
save('funding-boundary.json',dict(withdrawn=drain,capital_sat=capital,sole_input_txid=funding,returned_sat=1000000))
q(f"CREATE FUNCTION expiry_commit_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.id='{coin['id']}' THEN PERFORM pg_advisory_xact_lock(50328); END IF; RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER expiry_commit_hold AFTER INSERT ON expiry_settlement DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION expiry_commit_hold()")
lock=subprocess.Popen(D[:2]+['-i']+D[2:]+['psql','-X','-U','postgres','-d','expiry_task','-At'],
    stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(OUT/'commit-lock.log').open('w'),text=True)
try:
    lock.stdin.write("SELECT pg_advisory_lock(50328); SELECT 'locked';\n");lock.stdin.flush()
    while lock.stdout.readline().strip()!='locked':assert lock.poll() is None
    configure_task(enabled=True,max_batch=1)
    wait(lambda:int(q("SELECT count(*) FROM pg_stat_activity WHERE wait_event='advisory' AND query='COMMIT'"))>0,
        'original COMMIT reached PostgreSQL')
    stop_daemon('captaind',kill=True)
    assert row(coin['id']) is None
    assert int(q("SELECT count(*) FROM pg_stat_activity WHERE wait_event='advisory' AND query='COMMIT'"))>0
    config=F/'captaind.toml';config.write_text(config.read_text().replace('enabled = true','enabled = false'))
    with ThreadPoolExecutor(1) as pool:
        startup=pool.submit(start_daemon,'captaind')
        time.sleep(5)
        accepted_early=startup.done()
        barrier_wait=int(q("SELECT count(*) FROM pg_locks WHERE relation='nursery_tx'::regclass AND mode='ShareLock' AND NOT granted"))>0
        offboard=None
        if accepted_early:
            startup.result()
            offboard=bark(offboarder,'offboard','--vtxo',other['id'],'--no-sync',check=False)
        lock.stdin.close();lock.wait(timeout=5)
        startup.result(timeout=60)
    durable=wait(lambda:row(coin['id']),'old process COMMIT became durable')
    prior=rpc('decoderawtransaction',[durable['raw']])
    assert len(prior['vin'])==1 and prior['vin'][0]['txid']==funding,'payout did not select the sole test funding input'
    evidence=dict(payment=durable['txid'],sole_input_txid=funding,
        accepted_requests_before_commit_resolved=accepted_early,
        nursery_commit_wait_observed=barrier_wait)
    if offboard is not None and offboard.returncode==0:
        offboard_txid=json.loads(offboard.stdout)['offboard_txid']
        tx=rpc('getrawtransaction',[offboard_txid,True])
        inputs=lambda tx:{(i['txid'],i['vout']) for i in tx['vin']}
        overlap=inputs(prior)&inputs(tx)
        evidence.update(offboard=offboard_txid,conflicting_inputs=sorted(overlap))
        save('restart-commit-proof.json',evidence)
        assert not overlap,'restart spent inputs of an old process payout whose COMMIT completed later'
    # Resume the committed payment even though new payouts remain disabled.
    mine(1)
    wait(lambda:rpc('getrawtransaction',[durable['txid'],True]).get('confirmations',0)>0
        if durable['txid'] not in rpc('getrawmempool') else True,'late committed payout broadcast')
    mine(3)
    assert rpc('getrawtransaction',[durable['txid'],True])['confirmations']>=3
    assert barrier_wait,'startup did not wait for the old nursery writer'
    save('restart-commit-proof.json',dict(status='RECOVERY VERIFIED; final offboard check pending',**evidence))
    if offboard is None:
        offboard=bark(offboarder,'offboard','--vtxo',other['id'],'--no-sync')
        offboard_txid=offboard['offboard_txid']
        tx=rpc('getrawtransaction',[offboard_txid,True])
        assert funding not in [i['txid'] for i in tx['vin']]
        mine(1)
        assert rpc('getrawtransaction',[offboard_txid,True])['confirmations']>=1
        evidence['offboard_after_recovery']=offboard_txid
    save('restart-commit-proof.json',dict(status='PASS',**evidence))
finally:
    if lock.poll() is None:lock.stdin.close();lock.wait(timeout=5)
    q('DROP TRIGGER IF EXISTS expiry_commit_hold ON expiry_settlement; DROP FUNCTION IF EXISTS expiry_commit_hold()')
    if not listening(48535):start_daemon('captaind')
    returned=rpc('sendtoaddress',[address,(capital-1000000)/1e8,'','',False,True,None,'unset',None,3]);mine(3)
    save('funding-restored.json',dict(returned_remainder_sat=capital-1000000,txid=returned))
    configure_task(enabled=True,max_batch=100)
event('PASS',scenario='late old-process COMMIT survives restart and competing offboard funding')
