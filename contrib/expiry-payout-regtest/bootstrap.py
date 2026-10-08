from common import *
import shutil, re

F.mkdir(parents=True,exist_ok=True)
E.chmod(0o700)
cmd(['docker','exec','--user','0',PROJECT+'-postgres','chown','postgres:postgres','/wal-archive'],'wal-archive-owner')
(E/'wallets').mkdir(exist_ok=True)
if not (F/'captaind.toml').exists():
    (F/'captaind.toml').write_text((ROOT/'contrib/expiry-payout-regtest/captaind.toml').read_text().replace('__FIXTURE__',str(F)))
(F/'watchmand').mkdir(exist_ok=True)
# Core/Postgres are started only with this fixture's compose file.
wait(lambda: rpc('getblockcount') >= 0,'Core ready')
if 'faucet' not in rpc('listwallets'):
    rpc('createwallet',['faucet'])
if rpc('getblockcount') < 200:
    rpc('generatetoaddress',[200-rpc('getblockcount'),rpc('getnewaddress')])
if not (F/'captaind/mnemonic').exists():
    cmd([B/'target/debug/captaind','--config',F/'captaind.toml','create'],'create')
cmd(['docker','exec','-i',PROJECT+'-postgres','psql','-X','-U','postgres','-d','expiry_task','-v','ON_ERROR_STOP=1'],
    'expiry-schema',input=(B/'contrib/expiry-settlement.sql').read_text())
configured=(F/'captaind.toml').read_text()
if not (F/'watchmand.toml').exists():
    (F/'captaind.toml').write_text(configured.replace('enabled = true','enabled = false'))
if not listening(48535): start_daemon('captaind')
info=json.loads(cmd([B/'target/debug/captaind','--config',F/'captaind.toml','rpc','wallet'],'rounds-wallet').stdout)
address=(F/'sweep-address.txt').read_text().strip() if (F/'sweep-address.txt').exists() else info['rounds']['address']
(F/'sweep-address.txt').write_text(address+'\n')
s=(ROOT/'contrib/expiry-payout-regtest/watchmand.toml').read_text()
s=s.replace(s.splitlines()[0],f'sweep_address = "{address}"')
s=s.replace('"/data/watchmand"',f'"{F}/watchmand"').replace('"postgres"\nuser','"127.0.0.1"\nuser')
s=s.replace('password = "abandon-regtest"\n','').replace('port = 5432','port = 50432').replace('name = "bark-server-db"','name = "expiry_task"')
s=s.replace('"http://bitcoind:18443"','"http://127.0.0.1:53443"').replace('"127.0.0.1:3538"','"127.0.0.1:48538"')
s=s.replace('sweep_interval = "30s"','sweep_interval = "1s"').replace('reaction_interval = "10s"','reaction_interval = "1s"')
(F/'watchmand.toml').write_text(s)
if not (F/'watchmand/mnemonic').exists():
    shutil.copyfile(F/'captaind/mnemonic',F/'watchmand/mnemonic')
    (F/'watchmand/mnemonic').chmod(0o600)
if not listening(48538): start_daemon('watchmand')
if (F/'captaind.toml').read_text()!=configured:
    stop_daemon('captaind')
    (F/'captaind.toml').write_text(configured)
    start_daemon('captaind')
descriptor_row=wait(lambda: q("SELECT encode(content,'hex') FROM wallet_changeset WHERE kind='watchman' ORDER BY id LIMIT 1"),'watchman descriptor')
match=re.search(rb'tr\([^)]*\)',bytes.fromhex(descriptor_row))
assert match,'watchman descriptor absent'
descriptor=rpc('getdescriptorinfo',[match.group().decode()])['descriptor']
wa=rpc('deriveaddresses',[descriptor,[0,0]])[0]
if not (F/'funded.json').exists():
    r=rpc('sendtoaddress',[address,1,'','',False,True,None,'unset',None,3])
    w=rpc('sendtoaddress',[wa,.1,'','',False,True,None,'unset',None,3])
    (F/'funded.json').write_text(json.dumps(dict(rounds=r,watchman=w))+'\n')
    mine(3)

prime()
event('bootstrap-complete',tip=rpc('getblockcount'),rounds_address=address)
