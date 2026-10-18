"""Independent arithmetic over frozen decoded public records; no node or wallet access."""
from pathlib import Path
import hashlib
import json
from datetime import datetime, timezone

REPORT = Path('/home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017')
audit_path = REPORT / 'batch51-financial-audit-legacy.json'
audit_hash = 'ca8f1bd9f4ac19856a8e12e705492434d14669df948998f39c792b9e9d82cae9'
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
assert digest(audit_path) == audit_hash
audit = json.loads(audit_path.read_text())
assert audit['state'] == 'BOUNDED_BOTH_CASES_PUBLIC_CHAIN_RECONCILIATION_PASS'

# Rehash only the auditor's exact public inputs. No new inventory or private reads.
input_hashes = {}
for filename, expected in audit['input_sha256_before_and_after'].items():
    path = Path(filename)
    assert path.suffix in ('.json', '.py') or path.name in ('blk00000.dat', 'xor.dat')
    assert not any(part in ('postgres', 'wallets', 'backup') for part in path.parts)
    actual = digest(path)
    assert actual == expected, filename
    input_hashes[filename] = actual

def amounts(records):
    result = {}
    for item in records:
        point = item['outpoint']
        assert point not in result
        assert isinstance(item['amount_sat'], int) and item['amount_sat'] >= 0
        result[point] = (item['amount_sat'], item['script_hex'])
    return result

def recompute(case, phase_name, phase):
    boundary = amounts(phase['boundary'])
    frontier = dict(boundary)
    all_outputs = dict(boundary)
    consumed = set()
    fees = 0
    transactions = phase['decoded_DAG_transactions']
    by_id = {tx['txid']: tx for tx in transactions}
    assert len(by_id) == len(transactions)
    for tx in sorted(transactions, key=lambda t: (t['height'], t['position'])):
        assert tx['height'] <= phase['height']
        inputs = amounts(tx['vin'])
        outputs = amounts(tx['vout'])
        assert inputs and set(inputs) <= set(frontier)
        assert not set(inputs) & consumed
        for point, value in inputs.items():
            assert frontier.pop(point) == value
        for point, value in outputs.items():
            assert point.startswith(tx['txid'] + ':') and point not in all_outputs
            frontier[point] = value
            all_outputs[point] = value
        fee = sum(v[0] for v in inputs.values()) - sum(v[0] for v in outputs.values())
        assert fee >= 0 and fee == tx['actual_miner_fee_sat']
        fees += fee
        consumed.update(inputs)
    assert frontier == amounts(phase['terminal_frontier'])
    assert set(all_outputs) == set(phase['all_graph_outpoints'])
    initial = sum(v[0] for v in boundary.values())
    terminal = sum(v[0] for v in frontier.values())
    assert initial == terminal + fees
    assert (initial, terminal, fees) == (
        phase['selected_input_sat'], phase['terminal_output_sat'], phase['actual_miner_fee_sat'])
    payees = []
    for item in phase['payee_outputs_all_canonical_transactions']:
        found = amounts([dict(output, script_hex=item['script_hex'])
                         for output in item['canonical_outputs']])
        expected = item['expected_amount_sat']
        if expected is None:
            assert not found
        else:
            assert len(found) == 1 and sum(v[0] for v in found.values()) == expected
        assert all(all_outputs.get(point) == value for point, value in found.items())
        payees.append({'expected_sat': expected, 'canonical_output_count': len(found)})
    for package in phase['packages']:
        parent = by_id[package['parent_txid']]
        child = by_id[package['child_txid']]
        assert parent['height'] == child['height'] == package['same_block_height']
        assert parent['hash'] == child['hash']
        assert phase['height'] - parent['height'] + 1 == package['confirmations_at_phase'] >= 6
        assert parent['actual_miner_fee_sat'] == package['parent_fee_sat']
        assert child['actual_miner_fee_sat'] == package['actual_child_fee_sat']
        assert {v['outpoint'] for v in child['vin']} & {v['outpoint'] for v in parent['vout']}
    return {'case': case, 'phase': phase_name, 'height': phase['height'],
            'boundary_outpoints': len(boundary), 'transactions': len(transactions),
            'terminal_outputs': len(frontier), 'input_sat': initial,
            'terminal_sat': terminal, 'actual_miner_sat': fees,
            'all_declared_inputs_closed_unique': True, 'frontier_recomputed': True,
            'payee_counts_checked_against_reported_full_inventory_scan': payees}

results = []
for case in audit['cases']:
    for phase_name in ('first_epoch', 'final_epoch', 'final_cut'):
        if phase_name in case:
            results.append(recompute(case['case'], phase_name, case[phase_name]))
assert [(r['input_sat'], r['terminal_sat'], r['actual_miner_sat']) for r in results] == [
    (500000, 497719, 2281), (600000, 595438, 4562), (300000, 297719, 2281)]
assert digest(audit_path) == audit_hash
output = {'state': 'PARENT_INDEPENDENT_DECODED_RECORD_ACCOUNTING_RECOMPUTATION_PASS',
          'at': datetime.now(timezone.utc).isoformat(), 'audit_sha256': audit_hash,
          'recomputation_script_sha256': digest(Path(__file__)), 'cuts': results,
          'original_public_inputs_rehashed': input_hashes,
          'scope': 'Independent arithmetic, prevout closure, unique consumption, topological reachability, '
                   'frontier, fees and package confirmations over the frozen auditor decoded records. '
                   'Original public inputs rehashed, not decoded a second time. Whole canonical inventory, '
                   'headers/Merkle and raw-byte decoding inherit the independent auditor report. '
                   'Keys, database classification, SDK history, signing and ownership remain native provenance. '
                   'Successive H32 phase frontiers are not added. No new native run, RPC, service, '
                   'wallet/database access, custody, full legacy/ML or production qualification.'}
target = REPORT / 'batch51-financial-parent-recompute.json'
with target.open('x') as stream:
    json.dump(output, stream, indent=2)
    stream.write('\n')
print(json.dumps({'state': output['state'], 'report_sha256': digest(target),
                  'cuts': len(results), 'public_inputs_rehashed': len(input_hashes)}))
