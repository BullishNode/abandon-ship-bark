from common import *

configure_task(enabled=False)
_,small=board('fee-small',20000)
_,large=board('fee-large',90000)
expire([small,large])
try:
    configure_task(enabled=True,max_fee_pct=1,max_batch=1)
    big=settle([large])[0]
    ticks()
    assert row(small['id']) is None,'uneconomical oldest coin was claimed'
    assert q(f"SELECT spend_state FROM vtxo WHERE vtxo_id='{small['id']}'")=='spendable'
    mine(3)
finally:configure_task(enabled=True,max_fee_pct=20,max_batch=100)
small_proof=settle([small])[0]
mine(3)
save('fee-selection-proof.json',dict(large=big,small=small_proof))
event('PASS',scenario='actual percentage cap leaves small coin refreshable; later coin pays; restoring cap releases small coin')
