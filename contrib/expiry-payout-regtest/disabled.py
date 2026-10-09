"""Ordinary flows work with the task off and its optional tables unavailable."""
from common import *

configure_task(enabled=False)
stop_daemon('captaind')
renamed=False
try:
    # Preserve the existing qualified history. Only names change, atomically;
    # no table, payment, index or foreign key is deleted.
    q('BEGIN; ALTER TABLE expiry_settlement RENAME TO expiry_test_saved_settlement; '
      'ALTER TABLE expiry_cancelled_participation RENAME TO expiry_test_saved_cancelled; COMMIT;')
    renamed=True
    start_daemon('captaind')
    owner,coin=board('disabled',50000)
    bark(owner,'refresh','--delegated','--all')
    wait(lambda:q(f"SELECT spent_in_round FROM vtxo WHERE vtxo_id='{coin['id']}'"),'disabled-mode refresh')
    mine(3)
    replacement=bark(owner,'vtxos')
    assert replacement and coin['id'] not in [c['id'] for c in replacement]
    result=bark(owner,'offboard','--all')
    mine(3)
    assert rpc('getrawtransaction',[result['offboard_txid'],True])['confirmations']>=3
    assert q("SELECT to_regclass('public.expiry_settlement') IS NULL")=='t'
    assert 'expiry payout tick summary' not in (OUT/'captaind.log').read_text()
    save('disabled-proof.json',dict(status='PASS',coin=coin,replacement=replacement,offboard=result,
        scope='native task disabled, optional tables unavailable; normal board/refresh/offboard work'))
finally:
    if listening(48535):stop_daemon('captaind')
    if renamed:
        q('BEGIN; ALTER TABLE expiry_test_saved_settlement RENAME TO expiry_settlement; '
          'ALTER TABLE expiry_test_saved_cancelled RENAME TO expiry_cancelled_participation; COMMIT;')
    config=F/'captaind.toml';config.write_text(config.read_text().replace('enabled = false','enabled = true'))
    start_daemon('captaind')
event('PASS',scenario='disabled optional tables not required for ordinary board, refresh and offboard')
