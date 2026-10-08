"""Planner benchmark with synthetic spent history in session-local tables.

This measures candidate lookup only. Copied rows are not valid new entitlements,
are never visible to captaind, and establish no payment or user-capacity claim.
"""
from common import *
import re

count=int(os.environ.get('EXPIRY_HISTORY_ROWS','1000000'))
assert 10000<=count<=5000000
source=(ROOT/'server/src/database/expiry_settlement.rs').read_text()
query=re.search(r'let rows = self.query\("(SELECT v.vtxo_id, v.vtxo, v.expiry,.*?)",\s*&\[',source,re.S).group(1)
index=re.search(r'CREATE INDEX IF NOT EXISTS expiry_payout_candidates .*?;',
    (ROOT/'contrib/expiry-settlement.sql').read_text(),re.S).group(0)
# Reconstruct upstream indexes without copying the already-installed new index.
indexes=json.loads(q("SELECT json_agg(pg_get_indexdef(indexrelid)) FROM pg_index "
    "WHERE indrelid='public.vtxo'::regclass "
    "AND indexrelid != coalesce(to_regclass('public.expiry_payout_candidates'),0)"))
baseline_indexes='\n'.join(ddl.replace(' ON public.vtxo ', ' ON pg_temp.vtxo ')+';' for ddl in indexes)
tip=rpc('getblockcount')
for placeholder,value in [('$1','0'),('$2',"''"),('$3','144'),('$4',str(tip)),('$5','10000'),('$6','256')]:
    query=query.replace(placeholder,value)
save('history-acceptance.json',dict(synthetic_spent_rows=count,
    query=query,index=index,threshold_ms=1000,scope='session-local candidate SQL benchmark, not settlement'))
sql=f"""
\\set ON_ERROR_STOP on
CREATE TEMP TABLE vtxo (LIKE public.vtxo INCLUDING ALL EXCLUDING INDEXES);
{baseline_indexes}
INSERT INTO vtxo SELECT * FROM public.vtxo;
CREATE TEMP TABLE history_source AS SELECT * FROM public.vtxo WHERE spend_state='spent' LIMIT 1;
INSERT INTO vtxo SELECT (jsonb_populate_record(NULL::public.vtxo,
    to_jsonb(v)||jsonb_build_object('id', (SELECT max(id) FROM public.vtxo)+g,
    'vtxo_id','synthetic-history-'||g,'spend_state','spent'))).*
    FROM history_source v CROSS JOIN generate_series(1,{count}) g;
ANALYZE vtxo;
SELECT 'BASELINE';
EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {query};
SELECT 'BASELINE_IDS';
SELECT coalesce(json_agg(vtxo_id),'[]') FROM ({query}) candidates;
{index}
ANALYZE vtxo;
SELECT 'INDEXED';
EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {query};
SELECT 'INDEXED_IDS';
SELECT coalesce(json_agg(vtxo_id),'[]') FROM ({query}) candidates;
SELECT 'COUNTS';
SELECT json_build_object('rows',(SELECT count(*) FROM vtxo),
    'bytes',pg_total_relation_size('pg_temp.vtxo'));
"""
started=time.monotonic()
result=cmd(D[:2]+['-i']+D[2:]+['psql','-X','-U','postgres','-d','expiry_task','-At'],
    'synthetic-history-plan',input=sql,timeout=1800)
baseline_text=result.stdout.split('BASELINE\n',1)[1].split('\nBASELINE_IDS\n',1)
baseline=json.loads(baseline_text[0])[0]
baseline_ids=json.loads(baseline_text[1].split('\nCREATE INDEX\n',1)[0])
plan_text=result.stdout.split('INDEXED\n',1)[1].split('\nCOUNTS\n',1)
indexed=plan_text[0].split('\nINDEXED_IDS\n',1)
plan=json.loads(indexed[0])[0];indexed_ids=json.loads(indexed[1]);counts=json.loads(plan_text[1])
assert counts['rows']>=count
assert baseline_ids==indexed_ids, 'index changed candidate selection or order'
assert 'expiry_payout_candidates' in json.dumps(plan), 'candidate index not used'
save('history-proof.json',dict(status='PASS' if plan['Execution Time']<=1000 else 'FAIL',
    scope='synthetic spent history only; temporary tables destroyed when psql disconnects',
    baseline_ms=baseline['Execution Time'],baseline_plan=baseline,
    execution_ms=plan['Execution Time'],plan=plan,counts=counts,candidate_ids=indexed_ids,
    setup_and_query_seconds=time.monotonic()-started))
assert plan['Execution Time']<=1000,plan['Execution Time']
event('PASS',scenario='candidate query over synthetic spent history',rows=counts['rows'],
    execution_ms=plan['Execution Time'])
