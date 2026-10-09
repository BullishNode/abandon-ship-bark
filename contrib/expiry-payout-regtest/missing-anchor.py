"""A real SIGKILL leaves an unsigned persisted round before a later payable coin."""
from common import *

state=F/'missing-anchor-case.json'
if os.environ.get('REUSE_MISSING_ANCHOR')!='1':
    configure_task(enabled=False)
    owner,original=board('missing-anchor',90000)
    q("CREATE FUNCTION expiry_round_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(50322); RETURN NEW; END $$; CREATE TRIGGER expiry_round_hold BEFORE UPDATE OF funding_tx ON round FOR EACH ROW EXECUTE FUNCTION expiry_round_hold()")
    lock=subprocess.Popen(D[:2]+['-i']+D[2:]+['psql','-X','-U','postgres','-d','expiry_task','-At'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(OUT/'round-lock.log').open('w'),text=True)
    try:
        lock.stdin.write("SELECT pg_advisory_lock(50322); SELECT 'locked';\n");lock.stdin.flush()
        while lock.stdout.readline().strip()!='locked':assert lock.poll() is None
        bark(owner,'refresh','--delegated','--all')
        wait(lambda:int(q("SELECT count(*) FROM pg_stat_activity WHERE datname='expiry_task' AND wait_event='advisory' AND query LIKE '%UPDATE round SET funding_tx%'"))>0,'unsigned round committed; signed funding write held')
        funding=q(f"SELECT funding_txid FROM round WHERE id=(SELECT spent_in_round FROM vtxo WHERE vtxo_id='{original['id']}')")
        assert funding
        raw=q(f"SELECT encode(funding_tx,'hex') FROM round WHERE funding_txid='{funding}'")
        assert all(not i.get('txinwitness') for i in rpc('decoderawtransaction',[raw])['vin'])
        assert funding not in rpc('getrawmempool')
        replacement=json.loads(q(f"SELECT row_to_json(x) FROM (SELECT vtxo_id id,expiry expiry_height,anchor_point chain_anchor FROM vtxo WHERE anchor_point LIKE '{funding}:%' AND spend_state='unclaimed' AND amount>1000 LIMIT 1) x"))
        stop_daemon('captaind',kill=True)
        lock.stdin.close();lock.wait(timeout=5)
        q('DROP TRIGGER expiry_round_hold ON round; DROP FUNCTION expiry_round_hold()')
        start_daemon('captaind')
    finally:
        if lock.poll() is None:
            if not lock.stdin.closed:lock.stdin.close()
            lock.wait(timeout=5)
        q('DROP TRIGGER IF EXISTS expiry_round_hold ON round; DROP FUNCTION IF EXISTS expiry_round_hold()')
        if not listening(48535):start_daemon('captaind')
    mine(2)
    later_owner,later=board('later-valid-anchor',100000)
    assert replacement['expiry_height']<later['expiry_height']
    expire([later])
    state.write_text(json.dumps(dict(owner=owner,original=original,funding=funding,replacement=replacement,later_owner=later_owner,later=later),indent=2)+'\n')
case=json.loads(state.read_text())
configure_task(enabled=True)
ticks(5)
assert row(case['replacement']['id']) is None,'unsigned round cannot be paid'
proof=row(case['later']['id'])
save('missing-anchor-observation.json',dict(case=case,later_receipt=proof))
assert proof is not None,'missing funding anchor starves later eligible payout'
settle([case['later']]);mine(3)
event('PASS',scenario='SIGKILL leaves unsigned round; missing anchor waits while later valid coin pays',txid=proof['txid'])
