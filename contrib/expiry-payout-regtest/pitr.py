from common import *

original_config=(F/'captaind.toml').read_text()
configure_task(enabled=False)
owner,coin=board('pitr',140000)
expire([coin])
mark=str(time.time_ns())
backup='/tmp/expiry-base-'+mark
started=time.monotonic()
cmd(D+['pg_basebackup','-h','127.0.0.1','-U','postgres','-D',backup,'-Fp','-Xnone','--checkpoint=fast'],'basebackup')
configure_task(enabled=True)
settle([coin])
before=row(coin['id'])
wal=q('SELECT pg_walfile_name(pg_current_wal_lsn())')
q('SELECT pg_switch_wal()')
wait(lambda:q(f"SELECT last_archived_wal >= '{wal}' FROM pg_stat_archiver")=='t','payment WAL archived')
save('archive-boundary.json',dict(wal=wal,payment=before,lsn=q('SELECT pg_current_wal_lsn()')))
base=OUT/('base-'+mark)
cmd(['docker','cp',PROJECT+'-postgres:'+backup,str(base)],'copy-base')
volume=PROJECT+'-pitr-'+mark
container=volume
image='postgres@sha256:da788743d2060767375896de4d646f7576f5911461444b372616f19ea61db2ec'
cmd(['docker','volume','create',volume],'create-restore-volume')
# Recovered cluster uses copied base data plus retained WAL, never a logical dump.
cmd(['docker','run','--rm','--user','root','-v',str(base)+':/backup:ro',
    '-v',volume+':/restore','--entrypoint','sh',image,'-c',
    "cp -a /backup/. /restore/ && printf '%s\\n' \"restore_command = 'cp /wal-archive/%f %p'\" >> /restore/postgresql.auto.conf && touch /restore/recovery.signal && chown -R postgres:postgres /restore"], 'prepare-physical-restore')
cmd(['docker','run','-d','--name',container,'-p','127.0.0.1:50433:5432',
    '-v',volume+':/var/lib/postgresql/data','-v',PROJECT+'_wal:/wal-archive:ro',image],'start-recovered-postgres')
oldD=D.copy()
configs={name:(F/(name+'.toml')).read_text() for name in ['captaind','watchmand']}
try:
    def recovery_done():
        p=subprocess.run(['docker','exec',container,'psql','-U','postgres','-d','expiry_task','-Atc','SELECT NOT pg_is_in_recovery()'],text=True,capture_output=True)
        return p.returncode==0 and p.stdout.strip()=='t'
    wait(recovery_done,'physical WAL recovery',300)
    D[:]=['docker','exec',container]
    assert row(coin['id'])==before,'PITR lost or changed payment receipt'
    assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{coin['id']}'")=='spent'
    assert q(f"SELECT encode(tx,'hex') FROM nursery_tx WHERE txid='{before['txid']}'")==before['raw']
    stop_daemon('captaind');stop_daemon('watchmand')
    for name,text in configs.items(): (F/(name+'.toml')).write_text(text.replace('50432','50433'))
    start_daemon('captaind');start_daemon('watchmand')
    (F/'receipts'/(before['txid']+'.json')).unlink(missing_ok=True)
    settle([coin])
    ticks()
    assert row(coin['id'])==before
    assert q(f"SELECT count(*) FROM expiry_settlement WHERE id='{coin['id']}'")=='1'
    mine(3)
    proof=payment(coin['id'])
    save('pitr-proof.json',dict(proof=proof,base_backup=str(base),wal_through=wal,
        restore_container=container,restore_volume=volume,elapsed_seconds=time.monotonic()-started))
    event('PASS',scenario='physical base backup plus subsequent archived WAL; same native payment after restart',**proof)
finally:
    if listening(48535):stop_daemon('captaind')
    if listening(48538):stop_daemon('watchmand')
    cmd(['docker','stop',container],'stop-restored-cluster')
    D[:]=oldD
    for name,text in configs.items(): (F/(name+'.toml')).write_text(text)
    start_daemon('captaind');start_daemon('watchmand')
    # Both histories contain this same payment. No new payment is created on
    # the recovered clone. The original fixture resumes for unrelated cases.
    synced()
