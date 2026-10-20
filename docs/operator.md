# Fork operations

Use the server, watchman, client and plugins from the same qualified release.
This source candidate is not qualified for bitcoin mainnet. Complete the release
qualification before you accept customer deposits.
Read that release's installation notes and limits before accepting deposits.
The four GNU programs also require the release-pinned node and plugins.
Provision the `hold` plugin separately; the server package includes the patched
`cln-grpc` plugin, not the complete Lightning runtime.

Use the release-pinned complete Core Lightning node, `v26.06.6-abandon6`.
The node includes a six-confirmation delay for on-chain HTLC failure reports.
The ordinary upstream node does not provide this boundary. Keep its matching
subdaemons and plugins. Do not use subdaemon replacements or disable version
checks. A version string alone does not verify the installed program bytes.
Check the release checksums for the complete runtime.

This fork requires fresh fork databases. It does not convert experimental fork
stores. Keep existing stores and backups intact.

## Configure and start

Keep the server and watchman admin ports on loopback or a private management
network. Their admin RPC is not authenticated. Protect the PostgreSQL and
Lightning endpoints with the deployment's existing access controls.

Validate both configuration files before starting the services:

```sh
captaind check-config /path/to/captaind.toml
captaind check-config --watchman /path/to/watchmand.toml
```

The internal Lightning refund depth must be at least 6. The supported protocol
assumption excludes reorgs that remove 6 or more blocks. It includes reorgs that
remove 1 through 5 blocks. This assumption does not make bitcoin finality
absolute.

Start the services through their configured supervisors. The program commands
are `captaind -C /path/to/captaind.toml start` and
`watchmand -C /path/to/watchmand.toml start`. Preserve their original data
directories and database identities on restart. Starting a new identity is not
recovery of an existing server.

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

A successful database open is not a completed recovery drill. Verify a rightful
customer payout and an actual seed-restored spend. Test crash recovery
separately from orderly cold backup recovery.

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
