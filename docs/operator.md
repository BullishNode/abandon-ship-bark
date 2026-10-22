# Fork operations

Use the server, watchman and plugins from the same qualified release.
This source candidate is not production ready and is not qualified for bitcoin
mainnet. Complete the release qualification before you accept customer deposits.
Read that release's installation notes and limits. The portable bundle also
contains Bark and `barkd` as test helpers; it is not a client release. The GNU
programs require the release-pinned node and both qualified plugins.

Use the release-pinned complete Core Lightning node, `v26.06.6-abandon6`.
The node includes a six-confirmation delay for on-chain HTLC failure reports.
The ordinary upstream node does not provide this boundary. Keep its matching
subdaemons and plugins. Do not use subdaemon replacements or disable version
checks. A version string alone does not verify the installed program bytes.
Check the release checksums for the complete runtime.

The GNU programs alone are not a portable Lightning installation. Import the
release bundle's CLN, grpc, Core and hold runtime closures before starting CLN.
The hold plugin needs the matching Nix `libsqlite3.so`, `libpq.so`, OpenSSL and C++
runtime libraries. The qualified hold plugin has an empty RUNPATH. The host's
library installation alone does not provide its required Nix runtime. Load the
bundle's `cln-runtime.env` only in the CLN service. Use its matching Nix Core 31
`bitcoin-cli` for CLN subprocesses. A host executable under that environment
can mix incompatible loaders. Keep both bundled plugins configured as
important plugins. Do not replace their bytes to repair a missing
library. Follow `contrib/release/README.md` in the source and `INSTALL.md`
in the bundle.

Use a fresh database for a new installation. This fork does not convert earlier
experimental fork stores. Complete stores from the same supported schema can
reopen with their original registrations and obligations. Keep existing stores
and backups intact.

## Install from a release

The qualification networks are regtest, private signet and public signet. Do
not install this candidate on bitcoin mainnet. Use a dedicated service account
and separate state for each stack. Install PostgreSQL 17 and Nix for the
release architecture. The server and watchman share one database. A second
stack needs another database, separate identities and separate service ports.

Obtain the archive, its checksum and the full source commit through the trusted
release channel. The filename or checksum alone does not establish release
qualification. Verify and extract in an empty installation directory:

```sh
set -eu
sha256sum --check "$BUNDLE_ARCHIVE.sha256"
tar --extract --gzip --file="$BUNDLE_ARCHIVE"
cd "$BUNDLE_DIRECTORY"
sha256sum --check SHA256SUMS
for closure in ./*-closure.nar; do
  nix-store --import < "$closure" || exit 1
done
./bin/captaind --version
./bin/watchmand --version
./bin/bark --version
./bin/barkd --version
```

Run these commands in a shell that stops on error. The sidecar must name the
archive in the verification directory. Check that all four versions report
the release's exact source commit. Read `release-artifacts.json` for the CLN
and Core store outputs. Verify the CLN executable checksum and require
`v26.06.6-abandon6`. Keep the imported closures as Nix garbage-collection
roots for as long as the release or its rollback remains in use. Set the
deployment's `runtime-cln` symlink to that CLN output. Point `current` at the
complete release directory. Repeat the checksum readback through `current`.

The bundle must provide its matching `captaind.default.toml` and
`watchmand.default.toml`. A bundle without these templates needs correction
before an artifacts-only clean installation. Copy the templates into private
configuration files. Set absolute `data_dir` paths, the network, bitcoin RPC
endpoint and authentication, and the same PostgreSQL connection in both files.
Keep credentials out of release assets, shell history and public logs.

Configure CLN with both bundled plugins as `important-plugin` entries. Set
`bitcoin-cli` to the bundled Core store output's `bin/bitcoin-cli`. Set
`grpc-port` and `hold-grpc-port` to the stack's reserved ports. In the server's
`[[cln_array]]`, set the node grpc URI, server certificate, client certificate
and client key. Set its matching `hold_invoice` URI and certificate/key paths.
Protect both grpc endpoints from public access. Load `cln-runtime.env` only in
the CLN service environment.

Start the bitcoin node, PostgreSQL and CLN before application initialization.
Check their network and original identities. Validate the application configs
as described below. Run this once for a new empty server:

```sh
captaind -C /path/to/captaind.toml create
```

`create` initializes the database and writes `mnemonic` in the server data
directory. It does not generate a config file. There is no `watchmand create`
CLI command. The watchman must read the same seed phrase and database as its
server. Provision an owner-only copy of the original server `mnemonic` in the
watchman data directory through the local secret-custody procedure. Compare
the files locally without displaying them. Never generate another seed phrase
for the watchman. Retain a private backup before funding the wallets.

## Configure and start

The server and watchman refuse non-loopback admin addresses. Their admin RPC
is not authenticated. Restrict access to local processes. Use an SSH tunnel
for remote administration. Do not publish the admin port through a proxy.
Protect the PostgreSQL and Lightning endpoints with the deployment's existing
access controls.

The integration manager uses the TCP peer address for per-key IP and DNS
filters. It ignores `X-Forwarded-For` unless the peer IP appears in
`rpc.integration_trusted_proxies`. This list is empty by default and accepts
exact IPv4 or IPv6 addresses. There is no automatic trust for loopback or
private networks. Set this list in the TOML file; the environment loader does
not parse proxy IP arrays. To use a proxy chain, list every trusted proxy IP. Each
proxy must append its actual peer address or replace untrusted headers.
The daemon reads the chain from right to left and stops at the first
untrusted hop. A malformed header from a trusted proxy gives no client
address; a key with IP or DNS filters then refuses the request. Keep direct
access to the integration listener restricted. A proxy that replaces the
header with one verified client IP needs only its own IP in this list.

Validate both configuration files before starting the services:

```sh
captaind check-config /path/to/captaind.toml
captaind check-config --watchman /path/to/watchmand.toml
```

The internal Lightning refund depth must be at least 6. The supported protocol
assumption excludes reorgs that remove 6 or more blocks. It includes reorgs that
remove 1 through 5 blocks. This assumption does not make bitcoin finality
absolute. Qualification of the reorg boundary remains incomplete.

For the short public-Signet configuration, use `vtxo_lifetime = 16` or
longer. With two board confirmations and the native client's 12-block exit
margin, 16 leaves two blocks of slack. Never reduce the lifetime below 16;
boarding and act-by protection can immediately start exiting newly admitted
coins. If board confirmations or client exit margins increase, increase the
lifetime too. Keep the normal mainnet default of `vtxo_lifetime = 4320`. This
is not permission to deploy on mainnet.

Reserve fixed local service ports from the kernel's ephemeral client-port pool
with `net.ipv4.ip_local_reserved_ports`, merging existing reservations. A client
connection, including one in TIME_WAIT, can otherwise prevent a daemon from
binding its admin or integration port. Configure the supervisor to restart
unexpected clean exits as well as failures; a critical worker can shut the
process down with exit status zero. An explicit administrative stop must still
suppress restart. For systemd, use `Restart=always`; `systemctl stop` still
suppresses automatic restart.

Start the services through their configured supervisors. The program commands
are `captaind -C /path/to/captaind.toml start` and
`watchmand -C /path/to/watchmand.toml start`. Preserve their original data
directories and database identities on restart. Starting a new identity is not
recovery of an existing server.

## Configure fees and limits

The complete settings are in `server/captaind.default.toml`,
`server/watchmand.default.toml` and `server/src/config.rs` in the release source.
The bundle must include those matching TOML templates. Keep private deployment
copies outside the release directory. Change deployment policy in TOML and
validate it before restart. Server environment overrides use `BARK_SERVER__`,
for example
`BARK_SERVER__BITCOIND__COOKIE`. Watchman overrides use `WATCHMAND__`, for
example `WATCHMAND__POSTGRES__PORT`. Nested keys use `__`.
Inspect the actual supervisor environment as well as the files. Config
validation does not establish liquidity, throughput or safe deadlines.

| Area | Configuration keys and effect |
| --- | --- |
| Board fee | `[fees.board]`: `min_fee_sat`, `base_fee_sat`, `ppm`; charge the greater of the minimum and base plus rounded-up proportional fee. |
| Offboard fee | `[fees.offboard]`: `base_fee_sat`, `fixed_additional_vb`, `ppm_expiry_table`; includes a feerate charge for fixed bytes plus destination script bytes. Account for the actual miner fee separately. |
| Refresh fee | `[fees.refresh]`: `base_fee_sat`, `ppm_expiry_table`; charge each refresh, including repeated refreshes. |
| Lightning receive fee | `[fees.lightning_receive]`: `base_fee_sat`, `ppm`. |
| Lightning send fee | `[fees.lightning_send]`: `min_fee_sat`, `base_fee_sat`, `ppm_expiry_table`. `ln_max_fee_ppm` assigns a fraction of that customer fee to CLN's routing budget. Additional input value beyond principal plus the fee also increases that budget. |
| Amount admission | `min_board_amount`, `max_vtxo_amount`, `max_board_amount`, `max_offboard_amount`, `max_arkoor_amount`, `max_ln_send_amount`, `max_ln_receive_amount`, `max_round_amount`. Unset optional maxima are unbounded. Zero disables the corresponding operation for the operation maxima. |
| Tree and round geometry | `max_vtxo_exit_depth`, `max_arkoor_fanout`, `nb_round_nonces`, `round_interval`, `round_submit_time`, `round_sign_time`, `round_forfeit_nonces_timeout`. Geometry is not a measured capacity guarantee. |
| Chain timing | `vtxo_lifetime`, `vtxo_exit_delta`, `required_board_confirmations`, `min_trusted_confs`, `expiry_internal_refund_depth`, `require_board_funding_tx`, `allow_expired_arkoor`, `nursery_confirm_target_blocks`. Refund depth defaults to 6 and validation rejects lower values. |
| Lightning timing | `htlc_expiry_delta`, `htlc_send_expiry_delta`, `max_htlc_expiry_delta`, `max_user_invoice_cltv_delta`, `invoice_expiry`, `receive_htlc_forward_timeout`, `cln_xpay_timeout`, `cln_xpay_max_retry_for`. A retry deadline does not prove payment failure. |
| Receive admission and pool | `ln_receive_anti_dos_required`; `[vtxopool]`: `vtxo_targets`, `vtxo_target_issue_threshold`, `vtxo_lifetime`, `vtxo_pre_expiry`, `max_vtxo_exit_depth`. Reserve liquidity and test replenishment under the declared load. |
| Fee estimates | `[fee_estimator]`: `update_interval`, `history_duration`, `fallback_fee_rate_fast`, `fallback_fee_rate_regular`, `fallback_fee_rate_slow`, optional `max_fee_rate`; configure both daemons. A ceiling can delay confirmation during a fee spike. |
| Watchman policy | `[watchman]`: `reaction_interval`, `sweep_interval`, `progress_grace_period`, `claim_chunksize`, `incremental_relay_fee`, `min_cpfp_amount`, `expiry_fee_warning`. The warning defaults to `"10000 sat"` even when absent from the template. |
| Attribution and recovery | `expiry_owner_scan_chunk_size`, `expiry_owner_scan_workers`, `recovery_xpub_nb_keys`, `max_read_mailbox_items`; PostgreSQL `max_connections`, `connection_timeout_secs`, `idle_timeout_secs`. |
| Offboard sessions | `offboard_session_timeout`, `offboard_check_interval`, `offboard_acceptable_fee_rate_duration`; the check interval must not exceed the timeout. |

Expiry tables use `expiry_blocks_threshold` and `ppm`. Keep thresholds and
rates in ascending order. One million ppm is 100%. Use the server's quoted
fee for the exact input set. The offboard maximum cannot exceed the arkoor
maximum. A round maximum below an existing VTXO can prevent its refresh; the
client does not automatically split it to fit. Plan its emergency exit before
you reduce that limit.

The current fallback output fee is fixed in code at 1,000 sats. There is no
TOML key for it. Within an allocation, leaves with the same payout script
share one output fee. If gross minus the fee is below that script's dust
threshold, the entire group is forfeited and no output fee is recorded.
Unknown ownership is never this dust case. Miner fees come from the watchman
fee child, separately from customer outputs. There is no enforced operator
subsidy cap. Do not describe this candidate as having every fee configurable.
No setting establishes a guaranteed payout deadline or a qualified capacity.

## Supply watchman fees

Query the watchman's own admin address. Do not confuse its wallet with the
server's round wallet. For a watchman bound to the example port 3538:

```sh
captaind rpc --addr 127.0.0.1:3538 wallet
```

Do not add `-C` with the server configuration to this command: that setting
selects the server's admin address instead of `--addr`. Require a non-null
`watchman` response.

Use the returned `watchman.address` on the configured network to replenish the
watchman wallet. The response separates trusted balance and confirmed UTXOs
from unconfirmed UTXOs. Before relying on a fee input, independently check its
outpoint with the configured bitcoin node's `gettxout`, including mempool
spends. It must belong to the watchman wallet, meet its configured
`min_trusted_confs`, and remain available under its coin-selection rules.
Reserved or otherwise excluded outputs cannot supply the fee. The available
value must cover `watchman.min_cpfp_amount` and the actual package fee.
A confirmed-output list or balance total alone does not prove usability.

Alert on these warnings:

- `Expiry payouts need confirmed usable watchman funds`
- `Expiry splitting needs confirmed usable watchman funds`
- `Proposed emergency payout operator subsidy exceeds warning level`

The subsidy warning is not a spending cap. The operator pays miner fees. Do not
change customer payout amounts or delete an unpaid obligation to meet a fee
budget. After replenishment confirms, verify that queued payouts resume.

If a foreign transaction spends a payout fee anchor, retain its raw bytes,
fee, inputs and replacement history. Inspect parent and child together. Do not
delete the allocation or alter customer amounts. The foreign-child replacement
fix still needs qualification on the final release programs. If replacement
cannot progress, retain the unpaid obligation and escalate with the actual
package evidence.

## Monitor payment and ownership

Monitor the age and value of unpaid obligations, not only process health or
broadcast counts. Alert when either process falls behind the bitcoin node,
when the chain view remains invalid, when owner-search work stops progressing,
or when an expired customer remains unpaid beyond the declared operating
window. Inspect these read-only tables with a restricted database account:

- `expiry_chain_view` and `expiry_sync_ack`: completed chain views and rebuilds.
- `expiry_pending_key` and `expiry_scan`: unresolved keys and derivation work.
- `expiry_entitlement`: decision fences, gross amounts and fixed source facts.
  A fence alone does not identify a final payee or establish payment.
- `expiry_allocation_coin`: the allocated owner, script, disposition and amounts.
- `expiry_allocation_input`, `expiry_allocation`, `expiry_allocation_payout` and
  `expiry_fee_child`: backing inputs, signed parents, recipient outputs and fee
  children.
- `expiry_terminal_claim`: independently confirmed customer claims.
- `expiry_lightning_dispatch`, `expiry_lightning_external_success` and
  `expiry_lightning_terminal_failure`: particular external payment requests and
  their outcome evidence.

These records guide investigation. They do not by themselves establish
payment. Decode the retained signed transaction, identify the rightful script
and amount, and check its actual outpoint on the canonical chain. Check all
relevant previous payouts and customer claims for duplicates. A successful RPC,
a local paid status or a mempool entry is insufficient. Check customer amounts,
output fees, operator change and miner fees separately.

Unknown ownership must remain unresolved. Do not assign an unknown customer's
value to the operator. Preserve registered xpubs, address bindings and scan
progress. A wrong mailbox hint does not replace proof against the registered
xpub.

Automated fallback requires the final recipient's authenticated fallback
registration on the Ark server. The destination key must derive from that
registered xpub. The `/receives/invoice/for-address` endpoint validates the
address. It does not verify the recipient's fallback registration.

At expiry, keep bitcoin held if no registration matches. Also keep bitcoin
held while a registered owner's attribution scan is incomplete. Do not assign
the recipient's bitcoin to the provider or operator.

### Alert rules and first actions

Set and record deployment thresholds before admission. At minimum, observe
each daemon every minute and evaluate work after every new block. Set a
maximum unpaid age, reserve refill time, minimum free disk and WAL headroom,
and a maximum attribution backlog for the measured workload. These are
operator policy values, not new product settings or tested payout guarantees.
Use a read-only database role and retain private snapshots for comparison.

| Alert condition | First action and closure |
| --- | --- |
| Either daemon exits or its required plugin is absent | Inspect the exact supervisor unit and plugin logs. Restore the pinned bytes and runtime environment. Require both daemons and plugins to resume useful work. |
| Node tip advances but either daemon stays behind for 2 consecutive new blocks | Compare its `expiry_sync_ack` and stored block hash with the canonical node. Check RPC, disk and worker errors. Close only after both completed views reconcile. |
| `expiry_chain_view.invalid` persists across 2 completed daemon sync cycles | Preserve the generation and acknowledgements. Investigate rebuild errors. Do not clear the invalid flag or force a financial decision. |
| Owner-search backlog exists but its coverage makes no progress across 2 scheduled work cycles | Inspect `expiry_pending_key`, `expiry_scan` and owner-scan warnings. Check database/resource pressure and authenticated registrations. Keep unresolved bitcoin held. |
| A customer's unpaid age exceeds the declared window, or backlog/value grows across observations | Identify its VTXO, rightful owner, decision fence and actual canonical spend. Resolve the specific hold, fee shortage or chain issue. A healthy process or a new allocation does not close the alert. |
| Either confirmed-usable-funding warning occurs | Refill the watchman wallet. Check usable confirmed outpoints, then observe the queued payout confirm with correct customer amounts. |
| `Proposed emergency payout operator subsidy exceeds warning level` occurs | Check actual package feerate and reserve headroom. Increase the operating reserve if needed. The warning does not cap spending or authorize a reduced customer payout. |
| `Expiry payout remains pending`, readiness or owner-scan warnings recur without progress | Retain allocation/key identifiers and source-specific error records. Follow the relevant ownership, fee-child or Lightning procedure. Never delete a fence to clear an alert. |
| Disk/WAL headroom falls below the configured alert, PostgreSQL backup fails, or RSS/database size grows without bound | Escalate before storage exhaustion. Preserve WAL and authoritative history. Verify the next backup and a separate restore; do not prune financial history to clear the alert. |
| A canonical-chain money check finds a wrong amount, wrong script or duplicate payment | Suspend new admission under reviewed deployment controls. Preserve all stores and signed transactions. Keep protective claims available and reconcile every affected obligation before resumption. |

For each alert retain onset, affected value, oldest obligation, canonical tip,
process versions and the action taken. Close it with observed progress or
canonical transaction/accounting evidence. Do not close it from silence in logs.

## Backup and restore

Back up the complete PostgreSQL database, the server and watchman data
directories, and the original Lightning node's complete wallet, channel state
and payment history. Protect seeds and credentials separately from public
incident reports. Record compatible program and plugin versions.

Use a supported PostgreSQL backup with all required WAL, or an orderly cold
copy of the entire stopped cluster. Copying selected fallback tables or a live
PGDATA directory is not a complete backup. Do not replace current Lightning
channel state with an older copy as a routine restore step. Follow the original
node's channel-recovery requirements and preserve its payment history.

Disable duplicate service instances and automatic restarts before a restore.
Preserve the failed stores for investigation. Restore into separate paths.
Reopen with the same supported version and identity, then let both chain views
reconcile before relying on payout decisions. Reconcile registrations, customer
obligations, source-specific Lightning outcomes and canonical transactions.
The retained registry must cover customer registrations that the server already
accepted. Do not resume admission from an older backup that omits these records.
Recover them from intact PostgreSQL/WAL or authentic identical registration.
A missing record remains unresolved; it never makes customer bitcoin operator
bitcoin. An absent client cannot recreate its lost server registration for you.
Restoring a backup that predates commitments does not prove those commitments
never existed. A lost registry or pruned payment history cannot be recreated
from an empty result.

Use backup and restore tools from the running PostgreSQL major version. For
PostgreSQL 17 on Ubuntu, invoke `/usr/lib/postgresql/17/bin/pg_dump` and
`/usr/lib/postgresql/17/bin/pg_restore` explicitly, with the intended socket,
port and database. For a separate cluster beside the default 5432 cluster,
configure port 5433 explicitly in the tools and both daemon configurations.
The unqualified Ubuntu wrapper can choose a default PostgreSQL 16 cluster.
Its restore tool cannot read a 17 dump archive. Validate
the archive with the same absolute `pg_restore --list` before changing the
release symlink or restoring anything.

For a same-release rollback, stop the application and CLN services and take
the supported database backup and cold identity/CLN copies. Repoint the release
symlink, restore with the matching tools if the rehearsal requires it, and
reopen the same identities. A binary rollback does not itself require a
database restore. Never restore an older snapshot over newer commitments.
Compare every registration, balance field, confirmed/unconfirmed UTXO set and
source spend fence. The admin wallet RPC reveals a new receiving address on each call, so that
address alone is not a stable balance snapshot. Compare identity files locally
without printing their contents, including server/watchman seeds and the CLN
HSM secret. Keep absent customer wallets closed. Follow the
`ROLLBACK.md` bundled with the release.

A successful database open is not a completed recovery drill. Verify a rightful
customer payout and an actual seed-restored spend. Test crash recovery
separately from orderly cold backup recovery.

### Consistent backup and restore drill

1. Inventory registrations, unpaid obligations, pending allocations, confirmed
   payouts, held coins, balances, UTXO sets and source spend fences. Record
   source/program/plugin identities and the intended PostgreSQL host, port and
   database. Keep this inventory private.
2. Disable admission, request replay, duplicate service instances and automatic
   restarts. Stop the exact application and CLN units through the supervisor.
   Confirm they and their child processes have stopped. The bitcoin node and
   PostgreSQL may continue. Application/CLN cold copies must be from this same
   quiescent boundary.
3. Create a new private backup directory with mode 0700. With `umask 077`, take
   the complete database dump. Use the configured database owner or an account
   that can read every table. Use a protected password file or socket
   authentication, not a password in the command line. Run every command in
   a shell with `set -eu`; stop the drill on the first failed precheck:

   ```sh
   /usr/lib/postgresql/17/bin/pg_dump --format=custom \
     --host="$PGHOST" --port="$PGPORT" --username="$APP_DB_OWNER" \
     --dbname="$PGDATABASE" \
     --file="$PRIVATE_DUMP"
   /usr/lib/postgresql/17/bin/pg_restore --list "$PRIVATE_DUMP"
   ```

4. Copy the complete stopped server/watchman and CLN directories into private
   backup storage. Preserve modes and ownership. Include the original seeds,
   CLN HSM secret, channel/payment database, plugin state, credentials and
   private configuration. Include any configured plugin store outside those
   directories. For point-in-time recovery use a supported physical
   backup plus its complete WAL archive instead of this logical-dump route.
   Check private backup checksums and restore access without exposing secrets.
5. Preserve the original stores. Restore the dump to a new empty database and
   restore application state into fresh directories. Provision the original
   database roles and grants. Create the database as its administrator, then
   restore as the intended application owner:

   ```sh
   /usr/lib/postgresql/17/bin/createdb --host="$PGHOST" --port="$PGPORT" \
     --username="$PGADMIN" --owner="$APP_DB_OWNER" "$RESTORE_DATABASE"
   /usr/lib/postgresql/17/bin/pg_restore --single-transaction --exit-on-error \
     --no-owner --host="$PGHOST" --port="$PGPORT" \
     --username="$APP_DB_OWNER" --dbname="$RESTORE_DATABASE" "$PRIVATE_DUMP"
   ```

6. Exclude all original producers before any restored instance starts. For an
   isolated rehearsal, use the stopped original CLN state at the backup
   boundary. Never start an old channel snapshot against live peers after
   channels have advanced. A real CLN disaster must follow that original
   node's supported channel-recovery procedure; a generic old-file restore
   does not prove safe channel recovery.
7. Point private configs at the restored paths/database. Check identity files
   locally against the authentic backups. Validate configs and installed bytes.
   Start the original CLN identity and then both application daemons. Wait for
   both completed canonical chain views. Reconcile all registrations,
   obligations, balances, UTXOs and source-specific Lightning outcomes.
8. Require every previously accepted registration and unpaid obligation to
   survive. Require the pending allocation to complete exactly once, the held
   coin to remain held, and the next new payout to pay its rightful owner.
   Independently close customer principal, stated fees, operator remainder and
   actual miner fees on the canonical chain. An allocation status is not proof.
9. Perform the separate seed-only payout spend below. Keep the absent original
   wallet closed throughout. Retain the commands, exit results and public
   transaction facts with secrets removed. Correct any procedure that the
   drill cannot execute. An orderly drill does not qualify crash/WAL recovery.

### Authentic seed custody and seed-only spend

At wallet creation, retain the original seed phrase in protected custody on
the host that created it. Retain its network, derivation scheme, account
fingerprint and birthday height separately. Verify backup identity locally
against that original before the drill. Do not reconstruct a seed from observed
addresses, transaction data or another wallet. Do not move seeds between hosts
or include them in test reports, Git, chat or release bundles.

A seed-only drill starts from a fresh wallet without its old database, mailbox
or Ark server connection. Use an independent on-chain recovery wallet that
supports the original derivation. A displayed address or a discovered balance
is insufficient. Match the registered fallback script and actual payout
outpoint, then sign and broadcast a spend from that exact payout. Verify its
confirmation and exact change script. To prove change recovery, discover and
spend that change from another fresh seed-only wallet too. Account for every
fee. Server/watchman seed-only checks must use their own original derivation,
not a client's BIP84 or BIP86 path. A server seed does not replace the registry,
financial database or CLN channel state.

The Bark harness exposes `create --mnemonic`, `--birthday-height`, a network
flag and explicit chain-source settings, followed by `onchain utxos` and
`onchain send DESTINATION "AMOUNT sat"`. Its seed argument is command-line
input; use only a private local recovery procedure that keeps it out of
history, transcripts and other users' process views. For an offline Ark
recovery,
use a new empty `--datadir`, an explicit unreachable local `--ark` endpoint,
and `create --force` to permit creation without a server. `--force` can erase
an existing wallet; never point it at the original or any nonempty data
directory. Do not use wallet discovery alone as proof of a seed-restored spend.

### Server and watchman seed-only recovery

The server uses a BIP39 English seed phrase with an empty passphrase. Derive
the BIP32 master private key for the original network. The rounds wallet uses
`m/1856555996'/0/*`. The watchman fee wallet uses `m/38644432'/0/*`.
Each wallet has one Taproot key chain for both receipts and change. Its Core
descriptor is `tr(MASTER_XPRIV/1856555996h/0/*)` or
`tr(MASTER_XPRIV/38644432h/0/*)`, respectively. Equivalently, derive that
hardened child first and use `tr(WALLET_XPRIV/0/*)`. There is no `/1/*` change
chain and no BIP86 account path. Recover each daemon from its authentic seed
file. A separate watchman initialization can have a different seed.

Use the following procedure only on signet or regtest. It is a qualification
drill, with both daemons stopped and excluded from restart. Use a local Core
node with its complete unpruned history and private cookie authentication.
Do not import the old application database into Core. The seed does not restore
the registry, liabilities, locked outputs, VTXOs or CLN channel state. Reconcile
those separately. Spend only the operator outputs assigned to the drill. Do
not spend backing outputs or reserved fee inputs for outstanding obligations.

`server-seed-descriptors.py` in the bundle is an offline Python 3 stdlib helper.
In the source it is `contrib/release/server-seed-descriptors.py`. It accepts
the original owner-only seed file through stdin. It writes a new owner-only
import file in an owner-only directory, without printing the seed or private
descriptor. It serializes the BIP32 master key; Core derives the child keys.
It does not validate BIP39 words or their checksum. Verify original seed
identity locally, then require the recovered public funding scripts to match
the authentic recorded scripts before any spend. Never run `listdescriptors`
with private keys, enable RPC request tracing, or attach private recovery files
to an evidence report.

Choose `RANGE_END` to include every issued index, including change. Use the
public address/index inventory retained at creation and during operation.
Without that inventory, extend the scan until all independently known outputs
are found. A finite scan with an unknown gap is not proof of a complete wallet.
Choose `NEXT_UNUSED` beyond every issued index and within that scan range.
Use a new unused index for every change output. Keep the daemons stopped so
they cannot concurrently issue or spend those indices. The example values
below fit the native drill; replace them with the actual inventory limits.
The import rescans from genesis, so it does not depend on an old birthday or
application database.

The example uses signet. For regtest, replace both `-signet` and the helper's
`--network signet`, and require `regtest` in the chain check. Set `BUNDLE`,
`CORE_DATA` and `SEED_FILE` to local paths. `SEED_FILE` is the authentic seed
backup on its original host. The signet Core node must already be synchronized.

```bash
set -euo pipefail
set +x
umask 077
BUNDLE=/opt/abandon/current
CORE_DATA=/srv/abandon/bitcoin
SEED_FILE=/srv/abandon/captaind/mnemonic
KIND=rounds # Repeat the complete drill with KIND=watchman and its authentic seed.
RANGE_END=1023
NEXT_UNUSED=1000
RECOVERY=$(mktemp -d /srv/abandon/recovery.XXXXXXXX)
CORE_STORE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["runtime_store_paths"]["core"])' "$BUNDLE/release-artifacts.json")
CORE=("$CORE_STORE/bin/bitcoin-cli" -signet -rpcconnect=127.0.0.1 "-datadir=$CORE_DATA")
test "$("${CORE[@]}" getblockchaininfo | python3 -c 'import json,sys; print(json.load(sys.stdin)["chain"])')" = signet
NAME="$KIND-$(basename "$RECOVERY")"
"${CORE[@]}" -named createwallet wallet_name="$NAME" disable_private_keys=false blank=true descriptors=true
WALLET=("${CORE[@]}" "-rpcwallet=$NAME")
python3 "$BUNDLE/server-seed-descriptors.py" --network signet --wallet "$KIND" \
  --range-end "$RANGE_END" --next-index "$NEXT_UNUSED" \
  --output "$RECOVERY/import.json" < "$SEED_FILE"
"${WALLET[@]}" -stdin importdescriptors < "$RECOVERY/import.json" > "$RECOVERY/import-result.json"
python3 -c 'import json,sys; assert all(x["success"] for x in json.load(open(sys.argv[1])))' "$RECOVERY/import-result.json"
"${WALLET[@]}" listunspent 6 9999999 > "$RECOVERY/utxos.json"
```

Require the expected receipt outpoints, scripts and amounts in `utxos.json`.
Check each against Core `gettxout TXID VOUT true` and its confirmed transaction.
Select those exact inputs and an independently controlled destination. For
example, set `INPUTS='[{"txid":"ACTUAL_TXID","vout":0}]'`, `DESTINATION` to
that network's address, and `AMOUNT_SAT=50000` for a sufficiently large drill
output. These variables contain public transaction facts. The next commands
create a partial spend with explicit change on the same `/0/*` chain. They
keep the PSBTs and transaction data in private files and stdin too.

```bash
CHANGE=$("${WALLET[@]}" getnewaddress "" bech32m)
python3 - "$INPUTS" "$DESTINATION" "$AMOUNT_SAT" "$CHANGE" > "$RECOVERY/fund-input.txt" <<'PY'
import json, sys
print(sys.argv[1])
print(json.dumps([{sys.argv[2]: int(sys.argv[3]) / 100_000_000}]))
print(0)
print(json.dumps({"add_inputs": False, "changeAddress": sys.argv[4], "fee_rate": 2}))
PY
"${WALLET[@]}" -stdin walletcreatefundedpsbt < "$RECOVERY/fund-input.txt" > "$RECOVERY/funded.json"
python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["psbt"])' "$RECOVERY/funded.json" > "$RECOVERY/process-input.txt"
"${WALLET[@]}" -stdin walletprocesspsbt < "$RECOVERY/process-input.txt" > "$RECOVERY/signed.json"
python3 -c 'import json,sys; x=json.load(open(sys.argv[1])); assert x["complete"]; print(x["psbt"])' "$RECOVERY/signed.json" > "$RECOVERY/finalize-input.txt"
"${WALLET[@]}" -stdin finalizepsbt < "$RECOVERY/finalize-input.txt" > "$RECOVERY/finalized.json"
python3 -c 'import json,sys; x=json.load(open(sys.argv[1])); assert x["complete"]; print(x["hex"])' "$RECOVERY/finalized.json" > "$RECOVERY/broadcast-input.txt"
TXID=$("${WALLET[@]}" -stdin sendrawtransaction < "$RECOVERY/broadcast-input.txt")
"${CORE[@]}" getrawtransaction "$TXID" true > "$RECOVERY/transaction.json"
```

Wait for at least 6 confirmations. On regtest, mine 6 blocks to an independent
test address; on signet, wait for actual blocks. Require `gettxout` to show the
destination and the exact change output with at least 6 confirmations. Require
each original input to be spent. Compute the miner fee from the confirmed input
and output amounts and compare it with `funded.json`. Close input value =
destination value + change value + miner fee.

Unload the first wallet with `"${CORE[@]}" unloadwallet "$NAME"`. Preserve its
files. Repeat the import block with a new `RECOVERY` directory, new `NAME`,
and `NEXT_UNUSED` beyond the first change index. Give this second blank wallet
only the original seed and the scan range; do not import the first wallet or a
PSBT. Require it to discover the first confirmed change outpoint. Repeat the
spend block with `INPUTS` set to that exact change and a smaller `AMOUNT_SAT`.
Confirm the second spend, its destination and remainder. Close both miner fees
and the remainder against the original receipt amounts. Repeat for the other
wallet kind. Retain only public transaction facts as evidence.

The existing native test
`seed_wallet_recovery::server_and_watchman_seed_only_recover_receipts_and_change`
executes this procedure on regtest. It covers an advertised daemon address and
index 17, then change at index 1000 and its spend from a second fresh Core
wallet, for both wallet kinds. Run it through the qualification runner with
the final GNU binaries. A source check or a test on older binaries does not
qualify seed recovery for a new release.

## Upgrade and rollback

Keep each complete release in a separate directory. An upgrade requires an
explicitly qualified schema transition and matching CLN/plugins. This candidate
has no conversion for earlier experimental fork schemas. A source-level
compatibility statement does not qualify an upgrade.

Before a change, verify the new archive and readback. Record current identities,
registrations, obligations, balances and UTXO sets. Stop and exclude producers,
take the consistent private backup above, and retain the current release target.
Repoint `current` only to the reviewed compatible release. Preserve state and
identity paths. Validate configs, start the services and require both chain
views and independent financial checks to reconcile before admission resumes.

For rollback, follow `contrib/release/ROLLBACK.md` in the source or `ROLLBACK.md`
in the bundle. Do not run an older program against a newer schema without a
qualified compatibility path. A same-release rehearsal proves only reopen and
restore of that release. It does not prove downgrade, schema conversion, fresh
host recovery or crash atomicity. If no prior compatible release exists, retain
the stopped state and investigate; never initialize a replacement identity.

## Reorgs and unresolved Lightning requests

After a reorg, preserve the signed transactions and original payment evidence.
Allow the existing chain listeners to rebuild facts from both completed views.
Verify canonical transactions and fees again. Do not clear decision fences or
replace financial rows to resume service. If a reorg removes 6 or more blocks,
escalate the incident and suspend new admission and new refund issuance under
the deployment's reviewed controls. Preserve the stores, bitcoin node, Lightning
node and required protective claims. There is no verified single-command
incident halt. Stopping a service cannot retract an already signed or public
refund. Reconcile the affected Lightning payments and refunds before resuming.
The supported reorg assumption no longer applies.

A timeout, local Failed status, shared payment hash or empty Lightning history
does not prove that an external payment failed. Do not replay an ambiguous
request or import an invented node error. Recover the original node identity
and intact history, then reconcile the exact dispatch label and payment group.
Keep outstanding liabilities visible while investigation continues.

## Resolve a request that never reached Lightning

Use this procedure only for an external payment whose server dispatch is
committed but whose original node has no record of that dispatch. A request with
an actual HTLC must use its genuine outcome. Do not use an elapsed deadline,
shared payment hash, missing reply or current program version as failure proof.

1. Disable automatic restart and request replay for every original producer.
   Establish that the original server process, transport queues, Lightning node
   children and delayed requests have terminated. Record their actual process
   and service identities. Disabling a node in the server is insufficient.
2. Preserve the original stores and incident records. Reopen the original
   Lightning node with its intact history. Do not roll back or prune its store.
   Keep the old producers and request redelivery disabled throughout recovery.
3. Prepare the version-1 incident manifest from the retained operational records.
   Bind it to the original network, node, attempt, dispatch label, amount and
   complete sorted source set. Bind the existing supervisor exclusion file by
   its device and inode. Reference the retained quiescence, history-retention and
   no-redelivery records by absolute path and SHA256. Keep their bytes unchanged.
4. If another payment uses the same hash, include retained qualification of the
   original dispatcher, grpc plugin and xpay plugin. These records must precede
   the dispatch. They must establish the reviewed label propagation through the
   original source and executable versions. Current versions cannot replace
   missing original records. Without this qualification, the command requires
   the entire hash to have no raw or aggregate payment history.
5. Review the operational evidence independently. The program checks its shape,
   binding and byte identity. It cannot establish that the recorded events are
   true or that an uncooperative remote producer has stopped.
6. Run the local command with the original server configuration:

   ```sh
   captaind -C /path/to/captaind.toml resolve-uninjected-lightning \
     --manifest /path/to/incident.json --acknowledge-trusted-operations
   ```

7. Retain the command result and original records. The command checks the actual
   original node identity, network, version and unfiltered payment history. It
   rejects any own payment row in the qualified-label route. It records one
   source-specific resolution under the existing financial locks. It does not
   send a payment or retry the original request.
8. Resume normal observation with duplicate producers still excluded. Verify
   that the rightful customer eventually receives the fallback payout. Check
   the canonical outpoint, amount, script, fees and absence of another payment.
   Reimport of the same accepted incident is idempotent; it is not another refund.
   An identical accepted reimport returns before fresh node checks. Its result
   does not establish a new observation of node history or quiescence.

The JSON field definitions are in `server/src/ln/mod.rs`:
`SupervisedLightningBinding`, `SupervisedLightningManifest`, `IncidentEvidence`
and `OriginalLabelledProducer`. The manifest is at most 32KiB. Each referenced
record is at most 1MiB. Keep secrets out of these records. There is no automatic
incident-record generator or general operator permission to mark payments failed.
Loss of original history or failure to establish complete quiescence can prevent
this procedure. Preserve the liability and investigate; never invent evidence.

## Safe shutdown and operating limits

Use each deployment supervisor to stop its exact service instance. Disable
restart before an incident restore. Preserve current stores, WAL, original
Lightning history and pending signed transactions. Do not stop the bitcoin node
or remove protective claims merely because an application stops. A stopped
server does not cancel an in-flight Lightning request or retract a signed refund.

The operator must maintain usable fee reserves and chain observation. Monitor
actual completed work, backlog age and resource use. Synthetic registrations and
scheduled transfers do not establish throughput. Keep the deployment within the
load and tree geometry actually qualified in its release. No production capacity
claim follows from the current source fixtures. The current candidate still
requires financial, recovery and operational qualification.

The known same-owner fragmentation defect can forfeit a small leaf before its
siblings are ready. Its complete-group fix remains under qualification. Unknown
or incomplete ownership must remain held. A separate fee-replacement fix has a
passing targeted regression and scoped independent review, but the new release
build remains pending. See `RELEASE_NOTES.md` for the accepted large-tree
timing limit and the exact limits of its chain verification.
