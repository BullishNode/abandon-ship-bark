"""Assigned postterminal batch50 public receipts/retained Core bytes only; no actor/RPC/DB/wallet operations."""
from pathlib import Path
import ast,hashlib,json,struct
D=Path(__file__).parent
B=Path('/home/francis/.cache/abandon-fresh/testing-track-20261017/batch50-H30-ML125-v17')
sha=lambda b:hashlib.sha256(b).hexdigest()
helper=D/'batch23-upstream-bark-unstarted46-reconcile-core-blocks.py'
syntax=ast.parse(helper.read_text());functions=[n for n in syntax.body if isinstance(n,ast.FunctionDef) and n.name in {'digest','vi','tx'}]
exec(compile(ast.Module(body=functions,type_ignores=[]),str(helper),'exec'))
absence=D/'batch50-backend-actor-quiescence.json';assert sha(absence.read_bytes())=='99f0ea7047f59fd577738598016e958b51db26d99854bad873c929180c1b8689'
case_results=[]
for index,ctx,expected_receipt in [(1,'qualification_tiny_payout_grouping','9dc79beec10efbc03e3793c9f62436b767421df36b2e41c716c553e6d436e8a7'),(2,'qualification_offboard_reply_restore','c95ecfb02608b3e14f606e5537711be57a267cf955e00e689f505e6741cae441')]:
 context=B/f'case{index:02}'/'test-data/server'/ctx
 public=context/('tiny-group-public-receipt.json' if index==1 else 'offboard-reply-restore-public-receipt.json')
 receipt=json.loads(public.read_bytes());assert sha(public.read_bytes())==expected_receipt
 core=context/'bitcoind/regtest';files=[public,core/'blocks/xor.dat',*sorted((core/'blocks').glob('blk*.dat'))]
 before={str(p):sha(p.read_bytes()) for p in files};key=(core/'blocks/xor.dat').read_bytes();assert len(key)==8
 blocks={}
 for p in files[2:]:
  raw=p.read_bytes();data=bytes(v^key[i%8] for i,v in enumerate(raw));pos=0
  while pos+8<=len(data) and data[pos:pos+4]==bytes.fromhex('fabfb5da'):
   size=struct.unpack_from('<I',data,pos+4)[0];assert 80<size<=4000000
   blob=data[pos+8:pos+8+size];assert len(blob)==size;header=blob[:80];count,q=vi(blob,80);items=[]
   for _ in range(count):
    start=q;item,q=tx(blob,q);item['raw_sha256']=sha(blob[start:q]);items.append(item)
   assert q==len(blob);h=digest(header);assert h not in blocks
   blocks[h]={'hash':h,'prev':header[4:36][::-1].hex(),'txs':items};pos+=8+size
 children={b['prev'] for b in blocks.values()};tips=set(blocks)-children
 assert len(tips)==1,'no canonical-tip guess in a retained fork';tip=next(iter(tips));chain=[];cursor=tip
 while cursor!='0'*64:
  assert cursor in blocks;chain.append(blocks[cursor]);cursor=blocks[cursor]['prev']
 chain.reverse();assert len(chain)==len(blocks)
 tipheight=len(chain)-1
 if index==1:assert tip==receipt['canonical_tip_hash'] and tipheight==receipt['canonical_tip_height']
 txs={};locations={};spends={}
 for height,block in enumerate(chain):
  for item in block['txs']:
   tid=item['txid'];assert tid not in txs;txs[tid]=item;locations[tid]={'height':height,'hash':block['hash'],'confirmations':tipheight-height+1}
   for point in item['inputs']:
    if point[0]=='0'*64:continue
    assert point not in spends;spends[point]=tid
 def point(s):tid,v=s.rsplit(':',1);return tid,int(v)
 def output(p):return txs[p[0]]['outputs'][p[1]]
 def info(tid):
  item=txs[tid];inputs=[{'outpoint':p[0]+':'+str(p[1]),'amount_sat':output(p)[0],'script_hex':output(p)[1].hex()} for p in item['inputs']]
  fee=sum(i['amount_sat'] for i in inputs)-sum(v for v,_ in item['outputs']);assert fee>=0
  return {'txid':tid,**locations[tid],'raw_sha256':item['raw_sha256'],'inputs':inputs,'outputs':[{'outpoint':tid+':'+str(i),'amount_sat':v,'script_hex':s.hex(),'spent_by':spends.get((tid,i))} for i,(v,s) in enumerate(item['outputs'])],'actual_miner_fee_sat':fee}
 def sameblock(parent,child):
  a=info(parent);c=info(child);assert a['hash']==c['hash'] and min(a['confirmations'],c['confirmations'])>=6
  assert any(p[0]==parent for p in txs[child]['inputs']);return {'parent':a,'child':c}
 if index==1:
  assert receipt['phase']=='COMPLETE' and receipt['overall_pass'] is True
  external={point(receipt['root']):output(point(receipt['root']))}
  external.update({point(s):output(point(s)) for s in receipt['sponsors']});assert sorted(v for v,_ in external.values())==[100000]*3
  cut=dict(external);graph={};fees=0
  for block in chain:
   for item in block['txs']:
    if not any(p in cut for p in item['inputs']):continue
    assert all(p in cut for p in item['inputs']),'outside input enters selected finite cut'
    ins=sum(cut.pop(p)[0] for p in item['inputs']);outs=sum(v for v,_ in item['outputs']);assert ins>=outs
    fees+=ins-outs;graph[item['txid']]=item
    for vout,out in enumerate(item['outputs']):cut[(item['txid'],vout)]=out
  assert set(graph)==set(receipt['graph_txids']);assert sum(v for v,_ in cut.values())+fees==receipt['selected_finite_input_sat']==300000
  assert fees==receipt['actual_graph_miner_fee_sat'];assert sum(v for v,_ in cut.values())==receipt['remaining_output_sat']
  leaves=receipt['leaves'];tiny=[x for x in leaves if x['gross_sat']==500];sender=[x for x in leaves if x['gross_sat']==98500];assert len(tiny)==3 and len(sender)==1
  assert len({x['user_pubkey'] for x in tiny})==3 and len({x['script'] for x in tiny})==1 and len({x['outpoint'] for x in tiny})==1
  assert sorted((x['fee_sat'],x['payout_sat']) for x in tiny)==[(0,500),(500,0),(500,0)]
  payees=[]
  for row,amount in [(tiny[0],500),(sender[0],97500)]:
   script=bytes.fromhex(row['script']);matches=[(tid,i,v) for tid,item in txs.items() for i,(v,s) in enumerate(item['outputs']) if s==script]
   assert matches==[(point(row['outpoint'])[0],point(row['outpoint'])[1],amount)]
   p=point(row['outpoint']);assert p not in spends;assert locations[p[0]]['hash']==row['canonical_hash'] and locations[p[0]]['confirmations']>=6
   payees.append({'outpoint':row['outpoint'],'amount_sat':amount,'script_hex':script.hex(),**locations[p[0]]})
  packages=[sameblock(p['parent_txid'],p['child_txid']) for p in receipt['packages']]
  for package,original in zip(packages,receipt['packages']):assert package['parent']['actual_miner_fee_sat']==0 and package['child']['actual_miner_fee_sat']==original['actual_child_miner_fee_sat']
  terminal=[{'outpoint':p[0]+':'+str(p[1]),'amount_sat':v,'script_hex':s.hex(),'confirmations':locations[p[0]]['confirmations']} for p,(v,s) in cut.items() if v]
  assert all(p not in spends and locations[p[0]]['confirmations']>=6 for p,(v,s) in cut.items() if v)
  board=info(point(receipt['root'])[0]);assert board['actual_miner_fee_sat']==receipt['board_miner_fee_sat_outside_cut']==763
  assert sum(i['amount_sat'] for i in board['inputs'])==200000
  assert receipt['ordinary_customer_change_sat_outside_cut']==99237 and 100000+99237+763==200000
  result={'case':'H30','public_phase':'COMPLETE','tip_binding':'exact public receipt best-block hash/height; every retained block on that unique chain','canonical_tip':tip,'height':tipheight,'selected_input_sat':300000,'terminal_output_sat':sum(v for v,_ in cut.values()),'actual_graph_fee_sat':fees,'packages':packages,'payees':payees,'terminal_leaves':terminal,'board_outside_selected_cut':board,'three_public_fee_shares':[{'vtxo_id':x['vtxo_id'],'fee_sat':x['fee_sat'],'payout_sat':x['payout_sat'],'same_group_outpoint':x['outpoint']} for x in tiny],'limits':'Offline public bytes prove chain/cut/payee/fee values, not SDK history/DB ownership. COMPLETE plus actual nativePASS supplies returning-three-leaf/history/ten-bucket/idempotence assertions. This audit found exact one canonical output per publicpayee across all retained blocks, beyond source selectedgraph scope; no mempool decoder claim/globalcustody/fullML.'}
 else:
  tid=receipt['original_signed_txid'];offboard=info(tid);assert offboard['raw_sha256']==receipt['signed_bytes_sha256'];assert tid==receipt['original_unsigned_txid']
  assert offboard['height']==receipt['canonical_offboard_height'] and offboard['hash']==receipt['canonical_offboard_hash'] and offboard['confirmations']>=6
  assert offboard['actual_miner_fee_sat']==receipt['actual_offboard_miner_fee_sat']==1298
  payout=point(receipt['offboard_payout']);assert payout[0]==tid;net,script=output(payout);assert net==receipt['offboard_net_sat']==99146 and payout not in spends
  matching=[(t,i,v) for t,item in txs.items() for i,(v,s) in enumerate(item['outputs']) if s==script];assert matching==[(tid,payout[1],net)]
  assert net+receipt['committed_user_fee_sat']==100000
  # Offboard is funded by the operator wallet; customer discharge is an economic binding, not an offboard100k physical input.
  assert [i['amount_sat'] for i in offboard['inputs']]==[500000]
  assert sorted(x['amount_sat'] for x in offboard['outputs'])==[330,99146,399226]
  candidates=[t for t,item in txs.items() if sorted(v for v,_ in item['outputs'])==[99237,100000] and sum(output(p)[0] for p in item['inputs'])==200000]
  assert len(candidates)==1
  board=info(candidates[0]);assert board['actual_miner_fee_sat']==receipt['actual_board_miner_fee_sat']==763
  ordinary=[x for x in board['outputs'] if x['amount_sat']==99237];assert sum(x['amount_sat'] for x in ordinary)==receipt['ordinary_change_sat']==99237
  assert all(x['spent_by'] is None for x in ordinary);assert 99237+net+receipt['committed_user_fee_sat']+763==200000
  cp=point(receipt['positive_control_payout']);control=sameblock(cp[0],receipt['positive_control_child']);assert control['parent']['hash']==receipt['positive_control_parent_hash']
  assert control['parent']['actual_miner_fee_sat']==0 and control['child']['actual_miner_fee_sat']==receipt['positive_control_child_fee_sat']==2281
  assert point(receipt['positive_control_root']) in txs[cp[0]]['inputs'];assert output(point(receipt['positive_control_root']))[0]==20000
  assert output(cp)[0]==19000 and cp not in spends
  cs=output(cp)[1];assert [(t,i,v) for t,item in txs.items() for i,(v,s) in enumerate(item['outputs']) if s==cs]==[(cp[0],cp[1],19000)]
  sponsors=[{'outpoint':p,'amount_sat':output(point(p))[0],'spent_by':spends.get(point(p))} for p in receipt['watchman_prefunding']];assert all(x['amount_sat']==100000 for x in sponsors)
  result={'case':'ML125','canonical_tip':tip,'height':tipheight,'tip_binding':'all retained headers form one connected unbranched chain, with both receipt inclusion hashes/heights verified; no independent Corebeststate or RPC read','offboard':offboard,'customer_board_unique_native_shape_candidate':board,'customer_board_binding_limit':'Public capsule omits original source chainanchor/fullVTXO; unique retained200k→100k+99237 boardshape independently reconciled, originalsource association supplied by reached native assertions, not guessed from operator-funded offboard inputs','customer_ordinary_change':ordinary,'customer_equation_sat':[99237,99146,854,763],'customer_initial_sat':200000,'control_package':control,'control_payout':{'outpoint':receipt['positive_control_payout'],'amount_sat':19000,'script_hex':cs.hex()},'confirmed_prefunding':sponsors,'limits':'Public retained bytes verify actual original signedtx/chain/net/physicalfee and control20k→19k+sameblockchild. Committed854fee and prepare1/finish2/economicrow1/coldrestoration/ten-bucket/history/no-fallback-duplicate depend on receipt plus reached native assertions. OriginalfallbackSPK absent from publiccapsule; no privatewallet/DB inference or independentglobalfallbacknegative/globaloperatorcustody claim.'}
 after={str(p):sha(p.read_bytes()) for p in files};assert before==after
 result.update(public_receipt_sha256=expected_receipt,input_sha256_before_and_after=before,input_hashes_unchanged=True,public_only=True)
 case_results.append(result)
report={'state':'BOUNDED_PUBLIC_CORE_FINANCIAL_RECONCILIATION_PASS','native_source_manifest_sha256':'5d50a0f4dfd7a2fd61257363e5b29b013b6cdf48fdbb048f527e9f93f3fd178b','root_allocation_sha256':'423005848035ba599187a35a83fd342b523cddde52b2b1a088c0173824f96185','actor_quiescence_sha256':sha(absence.read_bytes()),'pure_decoder_source_sha256':sha(helper.read_bytes()),'cases':case_results,'operations':'Postterminal public JSON + public Core block XOR files only. No fullnative/debuglogs/argv/seed/config/wallet/SQL/PG/RPC/node/service/list/test/cleanup reads or actions. No replay, no current financial state mutation. No fullML or production qualification.'}
out=D/'batch50-public-Core-reconciliation-client.json';out.write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps({'state':report['state'],'receipt_sha256':sha(out.read_bytes()),'case_summaries':[{'case':r['case'],'height':r['height'],'packages':len(r.get('packages',[])),'input_hashes_unchanged':r['input_hashes_unchanged']} for r in case_results]}))
