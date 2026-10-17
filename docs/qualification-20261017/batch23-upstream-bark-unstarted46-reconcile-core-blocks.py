"""Read retained regtest block files only; no node, wallet or DB operations."""
import hashlib
import json
import struct
from pathlib import Path

BASE = Path(__file__).parent
CORE = BASE / 'test-data/fees/send_onchain_fee_deducted/bitcoind/regtest'
TIP = '0b6b678677e799a890f23f6294773e2d661a518c03f5a765e12bd783cc6cc269'
TXID = '50220caa0015059ae15b32468fc7913341c4e0d07a3204ad7b30c8715488818c'
SCRIPT = bytes.fromhex('00147a2a3f2ce33c2c6d316bdfcec0646ffbdfee4e66')

def digest(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()[::-1].hex()

def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()

def vi(b, p):
    n = b[p]
    p += 1
    if n < 253:
        return n, p
    size = {253: 2, 254: 4, 255: 8}[n]
    return int.from_bytes(b[p:p+size], 'little'), p+size

def tx(b, p):
    start = p
    version = b[p:p+4]
    p += 4
    witness = b[p:p+2] == b'\x00\x01'
    if witness:
        p += 2
    body_start = p
    count, p = vi(b, p)
    inputs = []
    for _ in range(count):
        prev = b[p:p+32][::-1].hex()
        index = struct.unpack_from('<I', b, p+32)[0]
        p += 36
        size, p = vi(b, p)
        p += size+4
        inputs.append((prev, index))
    count, p = vi(b, p)
    outputs = []
    for _ in range(count):
        value = struct.unpack_from('<Q', b, p)[0]
        p += 8
        size, p = vi(b, p)
        outputs.append((value, b[p:p+size]))
        p += size
    body_end = p
    if witness:
        for _ in inputs:
            count, p = vi(b, p)
            for _ in range(count):
                size, p = vi(b, p)
                p += size
    locktime = b[p:p+4]
    p += 4
    assert p <= len(b) and len(locktime) == 4
    return {'txid': digest(version+b[body_start:body_end]+locktime),
            'inputs': inputs, 'outputs': outputs, 'raw_size': p-start}, p

files = [CORE/'blocks/xor.dat', CORE/'debug.log', *sorted((CORE/'blocks').glob('blk*.dat'))]
before = {str(p.relative_to(CORE)): sha(p) for p in files}
key = files[0].read_bytes()
assert len(key) == 8
blocks = {}
for path in files[2:]:
    raw = path.read_bytes()
    data = bytes(v ^ key[i % 8] for i, v in enumerate(raw))
    p = 0
    while p+8 <= len(data):
        if data[p:p+4] != bytes.fromhex('fabfb5da'):
            # Bitcoin Core preallocates the unused remainder.
            break
        size = struct.unpack_from('<I', data, p+4)[0]
        assert 80 < size <= 4_000_000
        block = data[p+8:p+8+size]
        assert len(block) == size
        header = block[:80]
        count, q = vi(block, 80)
        transactions = []
        for _ in range(count):
            item, q = tx(block, q)
            transactions.append(item)
        assert q == len(block)
        blockhash = digest(header)
        blocks[blockhash] = {'hash': blockhash, 'prev': header[4:36][::-1].hex(), 'txs': transactions}
        p += 8+size

chain = []
cursor = TIP
while cursor != '0'*64:
    assert cursor in blocks, 'canonical ancestry absent from retained block files'
    item = blocks[cursor]
    chain.append(item)
    cursor = item['prev']
chain.reverse()
assert len(chain)-1 == 111
debug = (CORE/'debug.log').read_text()
assert f'new best={TIP} height=111' in debug
transactions = {t['txid']: t for block in chain for t in block['txs']}
matches = [(height, block, t) for height, block in enumerate(chain)
           for t in block['txs'] if t['txid'] == TXID]
assert len(matches) == 1
height, block, target = matches[0]
outputs = [{'vout': i, 'value_sat': value, 'script_hex': script.hex()}
           for i, (value, script) in enumerate(target['outputs']) if script == SCRIPT]
assert len(outputs) == 1 and outputs[0]['value_sat'] == 300_000
resolved = []
for prev, index in target['inputs']:
    value, script = transactions[prev]['outputs'][index]
    resolved.append({'txid': prev, 'vout': index, 'value_sat': value})
input_total = sum(i['value_sat'] for i in resolved)
output_total = sum(v for v, _ in target['outputs'])
assert input_total >= output_total
after = {str(p.relative_to(CORE)): sha(p) for p in files}
assert before == after
receipt = {
    'original_run_id': '352e12d4-ac04-40ce-b2e3-28401ceebf8b',
    'mode': 'retained block files and debug log, read-only; no process or wallet reopened',
    'canonical_tip': TIP, 'canonical_tip_height': 111,
    'offboard_txid': TXID, 'inclusion_block': block['hash'], 'inclusion_height': height,
    'confirmations_at_retained_tip': 112-height,
    'exact_recipient_outputs': outputs,
    'resolved_input_prevouts': resolved, 'input_total_sat': input_total,
    'all_outputs_total_sat': output_total, 'actual_miner_fee_sat': input_total-output_total,
    'input_hashes_before_and_after': before,
    'limitations': 'Bounded original transaction only. No six-confirmation, receiver wallet sync, later test assertion, whole-wallet solvency or adapted-source execution claim.'
}
destination = BASE/'retained-core-reconciliation.json'
destination.write_text(json.dumps(receipt, indent=2, sort_keys=True)+'\n')
print(json.dumps(receipt, indent=2, sort_keys=True))
