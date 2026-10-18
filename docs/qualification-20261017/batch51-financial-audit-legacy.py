"""Assigned postterminal public-capsule/retained-block audit. No RPC or private reads.

This is a bounded offline report writer, not an integration-test runner. Run only
after explicit parent terminal/scoped-quiescence admission. Never import a prior
audit entrypoint; reuse the three frozen pure decoder function ASTs only.
"""
from pathlib import Path
import ast
import hashlib
import json
import re
import struct
from datetime import datetime, timezone

REPORT = Path('/home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017')
BATCH = Path('/home/francis/.cache/abandon-fresh/testing-track-20261017/batch51-H32-H11-v19-v2')
ACTOR = REPORT / 'v19-H32-H11-postflight-backend-actors.json'
FORMAL = REPORT / 'v19-H32-H11-postflight-backend.json'
HELPER = REPORT / 'checkpoint23-final-safe/financial/batch23-upstream-bark-unstarted46-reconcile-core-blocks.py'
OUTPUT = REPORT / 'batch51-financial-audit-legacy.json'
sha = lambda data: hashlib.sha256(data).hexdigest()

assert not OUTPUT.exists(), 'new additive report only; never replace a frozen report'
assert sha(ACTOR.read_bytes()) == 'ae6ec940769d61310b74e76b865c4295fc23ebf1e98279af96093dc63ee1c300'
assert sha(FORMAL.read_bytes()) == '161a59128023f247289fff751bab620b3aaf87d20c6adce21f49a6aa16857ee3'
assert sha(HELPER.read_bytes()) == 'f88a7d9a116b3f59c924f4076dcb91d5b62c4590a0da4453c326ad253cadc146'
helper_tree = ast.parse(HELPER.read_text())
assert all(isinstance(node, (ast.Expr, ast.Import, ast.ImportFrom, ast.FunctionDef)) for node in helper_tree.body)
functions = [node for node in helper_tree.body if isinstance(node, ast.FunctionDef)]
assert [node.name for node in functions] == ['digest', 'vi', 'tx']
decoder_ast_hashes = {node.name: sha(ast.dump(node, include_attributes=False).encode()) for node in functions}
exec(compile(ast.Module(body=functions, type_ignores=[]), str(HELPER), 'exec'))


def outpoint(value):
    txid, index = value.rsplit(':', 1)
    assert re.fullmatch('[a-f0-9]{64}', txid)
    return txid, int(index)


def label(point):
    return point[0] + ':' + str(point[1])


def privacy_screen(raw):
    assert len(raw) < 100_000
    value = json.loads(raw)
    assert isinstance(value, dict)
    # These typed capsules contain public identifiers/amounts, never file contents.
    forbidden = {'mnemonic', 'seed', 'private_key', 'secret_key', 'password', 'admin_token',
                 'tls_key', 'wallet_data', 'database_data', 'backup_contents'}
    def walk(item):
        if isinstance(item, dict):
            assert not forbidden.intersection(key.lower() for key in item)
            for child in item.values():
                walk(child)
        elif isinstance(item, list):
            for child in item:
                walk(child)
    walk(value)
    assert b'-----BEGIN ' not in raw
    assert value['phase'] == 'COMPLETE' and value['overall_pass'] is True
    return value


def merkle(ids):
    level = [bytes.fromhex(value)[::-1] for value in ids]
    assert level
    while len(level) > 1:
        if len(level) % 2:
            level.append(level[-1])
        level = [hashlib.sha256(hashlib.sha256(level[i] + level[i + 1]).digest()).digest()
                 for i in range(0, len(level), 2)]
    return level[0]


def decode_chain(context, capsule):
    blockdir = context / 'bitcoind/regtest/blocks'
    paths = [blockdir / 'xor.dat', *sorted(blockdir.glob('blk*.dat'))]
    assert len(paths) >= 2
    inputs = {str(path): sha(path.read_bytes()) for path in paths}
    key = paths[0].read_bytes()
    assert len(key) == 8
    blocks = {}
    for path in paths[1:]:
        raw = path.read_bytes()
        data = bytes(value ^ key[i % 8] for i, value in enumerate(raw))
        offset = 0
        while offset + 8 <= len(data) and data[offset:offset + 4] == bytes.fromhex('fabfb5da'):
            size = struct.unpack_from('<I', data, offset + 4)[0]
            assert 80 < size <= 4_000_000
            block = data[offset + 8:offset + 8 + size]
            assert len(block) == size
            header = block[:80]
            count, cursor = vi(block, 80)
            transactions = []
            for _ in range(count):
                start = cursor
                transaction, cursor = tx(block, cursor)
                transaction['raw_sha256'] = sha(block[start:cursor])
                transactions.append(transaction)
            assert cursor == len(block)
            assert merkle([transaction['txid'] for transaction in transactions]) == header[36:68]
            bits = struct.unpack_from('<I', header, 72)[0]
            exponent, mantissa = bits >> 24, bits & 0x007fffff
            assert not bits & 0x00800000 and exponent >= 3
            target = mantissa << (8 * (exponent - 3))
            blockhash = digest(header)
            assert int(blockhash, 16) <= target
            assert blockhash not in blocks
            blocks[blockhash] = {'hash': blockhash, 'prev': header[4:36][::-1].hex(),
                                 'transactions': transactions}
            offset += size + 8
    tips = set(blocks) - {block['prev'] for block in blocks.values()}
    assert len(tips) == 1, 'do not guess a canonical tip in a retained fork'
    tip = next(iter(tips))
    chain, cursor = [], tip
    while cursor != '0' * 64:
        assert cursor in blocks
        chain.append(blocks[cursor])
        cursor = blocks[cursor]['prev']
    chain.reverse()
    assert len(chain) == len(blocks)
    transactions, locations, spends = {}, {}, {}
    for height, block in enumerate(chain):
        for position, transaction in enumerate(block['transactions']):
            txid = transaction['txid']
            assert txid not in transactions
            transactions[txid] = transaction
            locations[txid] = {'height': height, 'hash': block['hash'], 'position': position}
            for point in transaction['inputs']:
                if point[0] == '0' * 64:
                    continue
                assert point not in spends, 'duplicate canonical input'
                assert point[0] in transactions and point[1] < len(transactions[point[0]]['outputs'])
                spends[point] = txid
    # Receipt checkpoints bind the unique retained chain; no best-state DB/RPC read.
    checkpoints = []
    for prefix in ['first_core_tip', 'later_core_tip', 'canonical_tip', 'post_return_tip']:
        if prefix + '_height' in capsule:
            height = capsule[prefix + '_height']
            assert chain[height]['hash'] == capsule[prefix + '_hash']
            checkpoints.append({'name': prefix, 'height': height, 'hash': chain[height]['hash']})
    for path in paths:
        assert sha(path.read_bytes()) == inputs[str(path)]
    return {'chain': chain, 'txs': transactions, 'locations': locations, 'spends': spends,
            'input_hashes': inputs, 'tip': tip, 'height': len(chain) - 1, 'checkpoints': checkpoints}


def tx_record(chain, txid):
    transaction = chain['txs'][txid]
    inputs = []
    for point in transaction['inputs']:
        assert point[0] != '0' * 64
        amount, script = chain['txs'][point[0]]['outputs'][point[1]]
        inputs.append({'outpoint': label(point), 'amount_sat': amount, 'script_hex': script.hex()})
    outputs = [{'outpoint': label((txid, i)), 'amount_sat': value, 'script_hex': script.hex(),
                'spent_by_at_retained_tip': chain['spends'].get((txid, i))}
               for i, (value, script) in enumerate(transaction['outputs'])]
    fee = sum(item['amount_sat'] for item in inputs) - sum(item['amount_sat'] for item in outputs)
    assert fee >= 0
    return {'txid': txid, **chain['locations'][txid],
            'confirmations_at_retained_tip': chain['height'] - chain['locations'][txid]['height'] + 1,
            'serialized_bytes_sha256': transaction['raw_sha256'], 'vin': inputs, 'vout': outputs,
            'actual_miner_fee_sat': fee}


def ancestor_points(chain, txid):
    pending, seen, points = [txid], set(), set()
    while pending:
        current = pending.pop()
        if current in seen:
            continue
        seen.add(current)
        for point in chain['txs'][current]['inputs']:
            if point[0] == '0' * 64:
                continue
            points.add(point)
            pending.append(point[0])
    return points


def reconstruct(chain, capsule_cut, height, phase):
    roots, sponsors = capsule_cut['roots'], capsule_cut['sponsors']
    boundary = {}
    for kind, declarations in [('root', roots), ('sponsor', sponsors)]:
        for entry in declarations:
            point = outpoint(entry['outpoint'])
            assert point not in boundary
            value, script = chain['txs'][point[0]]['outputs'][point[1]]
            assert (value, script.hex()) == (entry['amount_sat'], entry['script'])
            assert chain['locations'][point[0]]['height'] <= height
            boundary[point] = (value, script, kind)
    assert all(not (set(boundary) - {point}) & ancestor_points(chain, point[0]) for point in boundary)
    frontier = {point: (value, script) for point, (value, script, _) in boundary.items()}
    all_points, spent, graph, fees = set(frontier), set(), [], 0
    for block in chain['chain'][:height + 1]:
        for transaction in block['transactions']:
            if not any(point in frontier for point in transaction['inputs']):
                continue
            assert all(point in frontier for point in transaction['inputs']), 'undeclared outside input into selected DAG'
            assert all(point not in spent for point in transaction['inputs'])
            total = sum(frontier.pop(point)[0] for point in transaction['inputs'])
            spent.update(transaction['inputs'])
            fee = total - sum(value for value, _ in transaction['outputs'])
            assert fee >= 0
            fees += fee
            graph.append(transaction['txid'])
            for i, output in enumerate(transaction['outputs']):
                point = transaction['txid'], i
                assert point not in all_points
                all_points.add(point)
                frontier[point] = output
    assert all(outpoint(entry['outpoint']) in spent for entry in roots)
    assert set(graph) == set(capsule_cut['graph_txids'])
    assert all_points == set(map(outpoint, capsule_cut['all_graph_outpoints']))
    nonzero = {point: output for point, output in frontier.items() if output[0]}
    declared = {outpoint(entry['outpoint']): (entry['amount_sat'], bytes.fromhex(entry['script']))
                for entry in capsule_cut['terminal_utxos']}
    assert nonzero == declared
    for point in nonzero:
        spender = chain['spends'].get(point)
        assert spender is None or chain['locations'][spender]['height'] > height
        assert height - chain['locations'][point[0]]['height'] + 1 >= 6
    initial, remaining = sum(value for value, _, _ in boundary.values()), sum(value for value, _ in nonzero.values())
    assert (initial, remaining, fees) == (capsule_cut['selected_input_sat'], capsule_cut['remaining_sat'], capsule_cut['actual_graph_miner_fee_sat'])
    assert initial == remaining + fees
    payees = []
    for expected in capsule_cut['expected_payees']:
        script = bytes.fromhex(expected['script'])
        # Scan every canonical transaction BEFORE considering selected DAG membership.
        matches = [(transaction['txid'], i, value) for block in chain['chain'][:height + 1]
                   for transaction in block['transactions'] for i, (value, output_script) in enumerate(transaction['outputs'])
                   if output_script == script]
        if expected['expected_amount_sat'] is None:
            assert not matches
        else:
            assert len(matches) == 1 and matches[0][2] == expected['expected_amount_sat']
            assert (matches[0][0], matches[0][1]) in nonzero
        payees.append({'script_hex': script.hex(), 'expected_amount_sat': expected['expected_amount_sat'],
                       'canonical_outputs': [{'outpoint': label((txid, index)), 'amount_sat': value}
                                             for txid, index, value in matches]})
    observed = {(entry['script_hex'], output['outpoint'], output['amount_sat']) for entry in payees for output in entry['canonical_outputs']}
    reported = {(entry['script'], entry['outpoint'], entry['amount_sat']) for entry in capsule_cut['canonical_payee_outputs']}
    assert observed == reported
    packages = []
    for package in capsule_cut['canonical_packages']:
        parent, child = package['parent_txid'], package['child_txid']
        assert parent in graph and child in graph
        p, c = tx_record(chain, parent), tx_record(chain, child)
        assert p['hash'] == c['hash'] == package['canonical_hash']
        assert p['height'] == c['height'] == package['canonical_height']
        assert height - p['height'] + 1 >= 6
        assert p['actual_miner_fee_sat'] == package['parent_miner_fee_sat'] == 0
        assert c['actual_miner_fee_sat'] == package['actual_child_miner_fee_sat'] > 0
        anchors = {entry['outpoint'] for entry in p['vout'] if entry['amount_sat'] == 0 and entry['script_hex'] == '51024e73'}
        assert len(anchors) == 1 and anchors.issubset({entry['outpoint'] for entry in c['vin']})
        packages.append({'parent_txid': parent, 'child_txid': child, 'same_block_height': p['height'],
                         'confirmations_at_phase': height - p['height'] + 1,
                         'parent_fee_sat': 0, 'actual_child_fee_sat': c['actual_miner_fee_sat']})
    operator = capsule_cut['operator_configured_script']
    watch = set(map(outpoint, capsule_cut['confirmed_watch_owned_terminal_points']))
    assert watch.issubset(nonzero)
    assert all(nonzero[point][1].hex() != operator and nonzero[point][1].hex() not in {item['script_hex'] for item in payees} for point in watch)
    return {'phase': phase, 'height': height, 'hash': chain['chain'][height]['hash'],
            'selected_input_sat': initial, 'terminal_output_sat': remaining, 'actual_miner_fee_sat': fees,
            'boundary': [{'outpoint': label(point), 'amount_sat': value, 'script_hex': script.hex(), 'kind': kind}
                         for point, (value, script, kind) in sorted(boundary.items())],
            'decoded_DAG_transactions': [tx_record(chain, txid) for txid in graph],
            'terminal_frontier': [{'outpoint': label(point), 'amount_sat': value, 'script_hex': script.hex(),
                                   'classification_from_capsule': 'declared operator script' if script.hex() == operator else
                                       'customer script' if script.hex() in {item['script_hex'] for item in payees} else 'native watch-wallet assertion',
                                   'confirmations_at_phase': height - chain['locations'][point[0]]['height'] + 1,
                                   'spent_after_this_phase_by': chain['spends'].get(point)}
                                  for point, (value, script) in sorted(nonzero.items())],
            'all_graph_outpoints': sorted(map(label, all_points)), 'payee_outputs_all_canonical_transactions': payees,
            'packages': packages, 'all_inputs_declared_unique': True, 'boundary_pairwise_nonoverlapping': True,
            'terminal_unspent_at_phase': True, 'watch_wallet_control_not_independently_verified': True}


cases = []
all_inputs = {str(ACTOR): sha(ACTOR.read_bytes()), str(FORMAL): sha(FORMAL.read_bytes()), str(HELPER): sha(HELPER.read_bytes())}
for index, name, receipt_name, expected_receipt in [
    (1, 'qualification_dust_later_receipt', 'dust-later-public-receipt.json', '35f5d95865460e1ed2a9fac63fe2b70d1d9301049d3060e1ea48be6fb309be16'),
    (2, 'qualification_three_owner_forwarding', 'three-owner-forwarding-public-receipt.json', 'aeb519daf61602a08b60de9320b3cfb1dba8859eae0c997a396aafbc5ae49900'),
]:
    context = BATCH / f'case{index:02}' / 'test-data/server' / name
    receipt_path = context / receipt_name
    raw = receipt_path.read_bytes()
    assert sha(raw) == expected_receipt
    capsule = privacy_screen(raw)
    chain = decode_chain(context, capsule)
    all_inputs[str(receipt_path)] = expected_receipt
    all_inputs.update(chain['input_hashes'])
    common = {'case': 'H32' if index == 1 else 'H11', 'public_capsule_sha256': expected_receipt,
              'capsule_phase': capsule['phase'], 'capsule_overall_pass': True,
              'retained_chain': {'height': chain['height'], 'tip_hash': chain['tip'], 'blocks': len(chain['chain']),
                                 'transactions': len(chain['txs']),
                                 'noncoinbase_transactions': sum(transaction['inputs'][0][0] != '0' * 64 for transaction in chain['txs'].values()),
                                 'receipt_bound_checkpoints': chain['checkpoints'],
                                 'unbranched_all_retained_blocks_connected': True, 'all_merkle_roots_and_header_PoW_match': True,
                                 'no_duplicate_canonical_inputs': True,
                                 'active_best_state_DB_or_RPC_read': False}}
    if index == 1:
        first = reconstruct(chain, capsule['first_cut'], capsule['first_core_tip_height'], 'first dust epoch')
        final = reconstruct(chain, capsule['final_cut'], chain['height'], 'final retained two-epoch cut')
        assert first['selected_input_sat'] == 500_000 and final['selected_input_sat'] == 600_000
        assert capsule['old_gross_sat'] == capsule['old_forfeited_sat'] == 1_200
        assert capsule['old_fee_sat'] == capsule['old_payout_sat'] == 0
        recipient_script = capsule['registered_recipient_script']
        assert capsule['new_registered_script'] == recipient_script and capsule['recipient_mailbox'] == capsule['new_owner_mailbox']
        assert capsule['new_vtxo_id'] != capsule['old_vtxo_id'] and capsule['new_user_pubkey'] != capsule['old_user_pubkey']
        assert capsule['new_gross_sat'] == 100_000 and capsule['new_fee_sat'] == 1_000 and capsule['new_forfeited_sat'] == 0
        assert capsule['later_payout_sat'] == 99_000
        script = bytes.fromhex(recipient_script)
        assert (len(script) == 22 and script[:2] == b'\x00\x14') or (len(script) == 34 and script[:2] == b'\x51\x20')
        dust = 3 * (8 + 1 + len(script) + 67)
        assert 0 < 1_200 - 1_000 < dust
        first_points = set(map(outpoint, first['all_graph_outpoints']))
        later_root = outpoint(capsule['new_chain_anchor'])
        assert later_root != outpoint(capsule['old_chain_anchor'])
        assert not first_points & ancestor_points(chain, later_root[0])
        assert chain['locations'][later_root[0]]['height'] > capsule['first_core_tip_height']
        final_by_script = {item['script_hex']: item for item in final['payee_outputs_all_canonical_transactions']}
        assert final_by_script[recipient_script]['expected_amount_sat'] == 99_000
        assert final_by_script[capsule['first_donor_registered_script']]['expected_amount_sat'] == 97_800
        assert len(final_by_script) == 3 and sum(len(item['canonical_outputs']) for item in final_by_script.values()) == 2
        boards = [tx_record(chain, outpoint(root)[0]) for root in [capsule['old_chain_anchor'], capsule['new_chain_anchor']]]
        for board, ordinary, fee in zip(boards, capsule['outside_cut_ordinary_change_sat'], capsule['outside_cut_board_miner_fees_sat']):
            assert sum(item['amount_sat'] for item in board['vin']) == 200_000
            assert board['actual_miner_fee_sat'] == fee and 100_000 + ordinary + fee == 200_000
        assert capsule['customer_principal_sat'] == capsule['customer_paid_sat'] + capsule['customer_policy_fee_sat'] + capsule['customer_forfeited_sat'] == 200_000
        assert capsule['customer_paid_sat'] == 196_800 and capsule['customer_policy_fee_sat'] == 2_000 and capsule['customer_forfeited_sat'] == 1_200
        common.update(first_epoch=first, final_epoch=final, outside_selected_cut_board_transactions=boards,
                      source_policy_values_from_reached_native_capsule={'old_gross_sat': 1200, 'old_fee_sat': 0, 'old_forfeited_sat': 1200,
                                                                        'later_gross_sat': 100000, 'later_policy_fee_sat': 1000,
                                                                        'combined_history_debit_sat': capsule['combined_fallback_debit_sat']},
                      actual_registered_script_dust_threshold_sat=dust, later_boundary_not_descended_from_any_first_graph_outpoint=True,
                      distinct_phase_frontiers_not_added_together=True,
                      independent_chain_outcome='No first-epoch recipient output; exactly one later99000 output, donor97800 and no later-donor payout; complete selected DAG conservation for both epochs.',
                      native_only='Signed VTXO/account/key bindings, BelowDust disposition/fee0/forfeit1200, actual old/new receipt acceptance, DB/history/all-ten-bucket/idempotence are reached native assertions. Public unsigned capsule labels do not independently prove those wallet or policy facts.')
    else:
        final = reconstruct(chain, capsule['selected_finite_cut'], chain['height'], 'final retained three-owner cut')
        assert final['selected_input_sat'] == 300_000
        owners = {owner['label']: owner for owner in capsule['owners']}
        assert set(owners) == {'A', 'B', 'C'} and len({owner['registered_script'] for owner in owners.values()}) == 3
        payees = {entry['script_hex']: entry for entry in final['payee_outputs_all_canonical_transactions']}
        assert payees[owners['A']['registered_script']]['expected_amount_sat'] is None
        assert payees[owners['B']['registered_script']]['expected_amount_sat'] is None
        assert payees[owners['C']['registered_script']]['expected_amount_sat'] == 99_000
        assert payees[owners['C']['registered_script']]['canonical_outputs'][0]['outpoint'] == owners['C']['payout_outpoint']
        assert capsule['final_gross_sat'] == capsule['final_payout_sat'] + capsule['final_policy_fee_sat'] + capsule['final_forfeited_sat'] == 100_000
        board = tx_record(chain, outpoint(capsule['source_chain_anchor'])[0])
        assert sum(item['amount_sat'] for item in board['vin']) == 200_000
        assert board['actual_miner_fee_sat'] == capsule['outside_cut_board_miner_fee_sat']
        assert 100_000 + capsule['outside_cut_customer_ordinary_change_sat'] + board['actual_miner_fee_sat'] == 200_000
        common.update(final_cut=final, outside_selected_cut_board_transaction=board, public_owner_identifiers=owners,
                      independent_chain_outcome='No canonical A/B registered-script output anywhere; exactly one unspent C99000 output; complete300k root/sponsor DAG and actual fee accounting.',
                      native_only='Two actual ordinary signed sends, complete successor proofs/account ownership, hydrated bodies, absent original clients, all-generation DB allocation exclusions and original A/B/C history/reopen/all-ten-bucket assertions remain reached native evidence. Do not add legitimate A/B send debits as duplicate economic payouts.')
    cases.append(common)

for path, expected in all_inputs.items():
    assert sha(Path(path).read_bytes()) == expected, 'public input changed during offline audit'
report = {'kind': 'BATCH51_INDEPENDENT_RETAINED_PUBLIC_CORE_FINANCIAL_AUDIT_LEGACY',
          'state': 'BOUNDED_BOTH_CASES_PUBLIC_CHAIN_RECONCILIATION_PASS',
          'at': datetime.now(timezone.utc).isoformat(),
          'source_manifest_sha256': '7d7c5b456d168d33223c9fe8b1daef8f2dbfbce33819a09af881e05d55ad795e',
          'runtime_metadata_sha256': '293c92d3e49b224703afc48a1acf82ad5b3c98e00cd7ec8dbf552a833e476f1f',
          'actor_quiescence_sha256': sha(ACTOR.read_bytes()), 'formal_postflight_sha256': sha(FORMAL.read_bytes()),
          'audit_script_sha256': sha(Path(__file__).read_bytes()),
          'pure_decoder_provenance': {'public_helper_sha256': sha(HELPER.read_bytes()), 'function_AST_sha256': decoder_ast_hashes,
                                      'only_digest_vi_tx_function_ASTs_executed': True, 'old_hardcoded_entrypoints_excluded': True},
          'cases': cases, 'input_sha256_before_and_after': all_inputs, 'all_read_inputs_unchanged': True,
          'limits': ['Public receipts and exact retained blk/XOR files only, after terminal/actorzero; no raw native logs, private wallet, DB, config, seed, backup, RPC, service, signing, restart, container, test or build access.',
                     'Selected boundary ownership and watch/operator custody are native/capsule assertions, not independently derived private keys. Header PoW/merkle/structure and actual values/prevouts/fees were checked; not a complete Bitcoin Script or signature-consensus revalidation.',
                     'No persisted mempool interpretation, active Core best-state database/RPC read, global operator/setup-funding cut, full legacy/ML, production or release qualification.',
                     'Phase-specific frontiers are successive states, not additive funds. Ordinary board funding/change/miner fees are separate from selected roots/sponsors.']}
OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps({'state': report['state'], 'report_sha256': sha(OUTPUT.read_bytes()),
                  'cases': [{'case': item['case'], 'retained_height': item['retained_chain']['height']} for item in cases]}))
