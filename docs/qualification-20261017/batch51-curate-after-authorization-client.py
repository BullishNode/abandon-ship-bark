"""Narrow batch51 coverage append. NOT authorized or executed at preparation.
Input is a separately reviewed parent authorization, actual audit hashes/results,
and complete coverage rows. Never derives PASS or ML acceptance from a title.
"""
from pathlib import Path
import csv,hashlib,io,json,sys
D=Path('/home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017')
assert len(sys.argv)==2, 'supply actual parent curation authorization only after terminal/formal/financial audits'
gatepath=Path(sys.argv[1]);gate=json.loads(gatepath.read_text())
assert gate['state']=='PARENT_EXCLUSIVE_BATCH51_CURATION_AUTHORIZED_AFTER_TERMINAL_AND_AUDITS'
assert gate['writer']=='client_test_gaps' and gate['other_shared_writers_absent'] is True
assert gate['parent_terminal_confirmed'] is True and gate['formal_scoped_actors_absent'] is True
assert gate['source_manifest_sha256']=='7d7c5b456d168d33223c9fe8b1daef8f2dbfbce33819a09af881e05d55ad795e'
sha=lambda b:hashlib.sha256(b).hexdigest()
for role in ['native','formal','financial']:
 ref=gate['audits'][role];raw=Path(ref['path']).read_bytes();assert sha(raw)==ref['sha256'],role
assert gate['financial_limits_reviewed'] is True and gate['ML_full_acceptance_changes']==0
names=['qualification_dust_later_receipt::forfeited_small_receipt_does_not_revive_when_same_customer_receives_again','qualification_three_owner_forwarding::only_final_owner_receives_after_two_full_value_forwards']
ids=['FT-SERVER-020','FT-SERVER-021'];results=gate['actual_results'];assert 1<=len(results)<=2
assert [r['name'] for r in results]==names[:len(results)]
assert all(r['binary']=='ark-testing::server' and r['result'] in ['PASS','FAIL'] and r['run_id'] and r['native_seconds']>0 for r in results)
assert all(r['result']=='PASS' for r in results[:-1]), 'firstFAIL must stop; preserve unstarted cases'
rows=gate['coverage_rows'];assert len(rows)==len(results)
coverage=D/'COVERAGE.csv';before=coverage.read_bytes()
assert sha(before)=='2f555fa380782909329df1da68469628aa3dac79992e518c91c05c0b32049371'
existing=list(csv.DictReader(io.StringIO(before.decode())));assert len(existing)==466 and before.endswith(b'\n')
header=list(existing[0]);existing_ids={r['case_id'] for r in existing}
for index,(row,result) in enumerate(zip(rows,results)):
 assert set(row)==set(header) and row['case_id']==ids[index] and row['case_id'] not in existing_ids
 assert row['automated_test_name']=='ark-testing::server '+result['name']
 assert row['latest_result']==result['result'] and row['tested_revision']==gate['source_manifest_sha256']
 assert row['entry_kind']=='executed_named_test' and row['owner']=='testing-track'
 assert result['run_id'] in row['success_oracle'] and str(result['native_seconds']) in row['success_oracle']
 assert row['limitation'] and row['evidence_path'] and row['success_oracle']
buf=io.StringIO(newline='');writer=csv.DictWriter(buf,fieldnames=header,lineterminator='\n');writer.writerows(rows)
addition=buf.getvalue().encode();after=before+addition
assert after[:len(before)]==before and len(list(csv.DictReader(io.StringIO(after.decode()))))==466+len(results)
# This mutation remains forbidden until a separately supplied explicit parent lease.
coverage.write_bytes(after)
receipt={'state':'AUTHORIZED_APPEND_ONLY_ACTUAL_BATCH51_ROWS','parent_authorization_sha256':sha(gatepath.read_bytes()),'prefix_sha256':sha(before),'prefix_bytes':len(before),'prefix_rows':466,'all_prior466_bytes_and_FAILs_preserved':True,'append_rows':len(rows),'row_ids':[r['case_id'] for r in rows],'coverage_sha256_after':sha(after),'audits':gate['audits'],'actual_results':results,'aggregate':{'case_executions':255+len(results),'passed_case_executions':240+sum(r['result']=='PASS' for r in results),'failed_case_executions':15+sum(r['result']=='FAIL' for r in results),'unique_executed_test_names':235+len(results),'nextest_invocations':228+len(results),'native_case_executions':243+len(results),'component_case_executions':12,'coverage_rows':466+len(results)},'scope':'Actual2new exact binary/name pairs only if started; no unstarted row or wholeML promotion. STATUS/FINDINGS not mutated by this script.'}
(D/'batch51-coverage-append.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({'appended':len(rows),'ids':receipt['row_ids'],'coverage_sha256':receipt['coverage_sha256_after']}))
