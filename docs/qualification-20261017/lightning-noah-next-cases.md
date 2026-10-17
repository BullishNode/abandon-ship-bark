# Stock Noah: strict prepayment gate and offline delivery interruption

Read-only source investigation and one source-only native prerequisite test. No callback, invoice request, payment, service action, build, Cargo, metadata or runtime test was executed. Restore draft remains frozen. Lightning grant/first-dispatch races belong to the other Lightning worker; this report covers receive claim registration, external-recipient delivery and strict stock-Noah callback compatibility.

Inspected stock Noah checkout `/home/francis/noah-fresh-backend-20261016`, clean HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`, including AGENTS.md. Product worktree HEAD `5635b5a95b636894b1e1aae9117eac3d057329a9`; current qualification additions are uncommitted test-only source. Simulator checkout observed HEAD `17103e999e744a2bdb460acca1e1fe9f77dfb30d`; its separate uncommitted source belongs to its owner and was not changed. Coordinator STATUS dated15:22UTC reports forwarding disabled and no actual stock-Noah receive; that is a dated report, not a new live observation here.

## Immediate stock-Noah gate: SOURCE-PREDICTED incompatibility

The current source path carries a plain memo, with no metadata-hash parameter:

- Noah `server/src/routes/public_api_v0.rs:381–458` builds exact LNURL metadata bytes, then passes `Some("Paying satoshis to user@domain")` into its forwarding provider.
- `server/src/barkd_client.rs:115–121,167–220` posts JSON `address`, `amount_sat`, `description` to `api/v1/lightning/receives/invoice/for-address`. It checks returned invoice network and exact amount, but no LNURL metadata description hash.
- Product `bark-rest/src/api/v1/lightning.rs:99–111` calls `bolt11_invoice_for_address`; `bark/src/actions/lightning/receive.rs` passes the plain string through `StartLightningReceiveRequest.description`.
- `server/src/ln/node_manager.rs:551–570` explicitly maps that string into hold-plugin `Description::Memo`.

Thus a plain-description invoice, rejected by the existing independent strict verifier, is source-predicted. No actual original callback invoice was fetched here, so this is **SOURCE-PREDICTED**, not an observed live compatibility FAIL. Do not pay it, alter returned metadata, substitute another invoice, relax hashing, or modify the stock Noah library/fork to create a passing result.

Smallest next actual step is one coordinator-allocated prepayment probe: freeze one authentic metadata response and one original amount-callback response, without `ark` request negotiation, before dispatch. The amount-callback GET creates an invoice via Noah's provider POST; it is stateful despite GET and must be journaled once, with no automatic retry after timeout/unknown reply. Metadata-only GET is different. Root owns profile/backend admission and actual probe execution.

Use the existing offline verifier binary `/home/francis/.cache/noah-public-proof-20261016/debug/noah-public-proof`, independently read SHA256 `f96c08217c085f99dad0e6c27c93d480f10314b7683408d73a3d27cfdc57e18c`. This is a current local file hash, not an inferred VM hash or source-build equivalence. Recheck exact admitted location/hash; do not rebuild it or install another verifier. It accepts stdin JSON exactly:

```json
{"invoice":"ORIGINAL_PR","amount_msat":10000000,"metadata":"ORIGINAL_METADATA_STRING_BYTES","network":"signet","now":0}
```

`now` must be actual observation Unix time, not0. A prepared offline invocation is `VERIFIED_BIN < PRIVATE_FROZEN_POLICY_JSON > PRIVATE_RESULT_JSON`; `NativeInvoiceVerifier` in `simulator/noah.py:221` already verifies the executable hash, bounds the command to10seconds, and captures original checker output. Its strict `InvoicePolicy` checks signature, exact whole-sat amount, Signet/Regtest network, expiry/future timestamp, and SHA256 of the unchanged metadata string. Its five offline units are not an actual callback result.

The smallest parser/observer adds no HTTP stack: consume already frozen original metadata/callback bodies plus an immutable declared recipient, amount, request URLs, observation time and binary pin; validate callback exact controlled HTTPS origin/path, `payRequest`, advertised bounds, whole sats and exact `text/identifier`; obtain nonempty original `pr`; pass the exact parsed metadata STRING (never reserialize metadata's internal JSON) to existing NativeInvoiceVerifier. Freeze invoice/payment hash before any payment. Preserve verifier nonzero exit/output privately as compatibility FAIL; timeout is unknown callback outcome and must not fetch another invoice automatically.

Use the retained private CA and strict TLS validation; no `curl -k`, redirects, alternate origin or public RPC exposure. Omit `ark` in both actual request URLs to avoid direct Ark bypass. Do not reject merely because the callback response includes an `ark` field: stock Noah returns that field alongside a genuine nonempty invoice even on the Lightning path. Store private raw HTTP/verifier evidence; public receipt contains hashes, recipient ID, amount, payment hash, timing and exact failure class. Never publish bearer/preimage/seed/raw wallet data.

## Native boundaries actually available

`bolt11_invoice_for_address` documentation and implementation in `bark/src/lightning/receive.rs:128–164` build claims directly against the destination policy. `prepare_lightning_receive_claim` uses `claim_destination.policy`; `claim_lightning_receive_htlcs` reveals the original preimage, cosigns outputs, calls actual `register_vtxo_transactions_with_server`, validates outputs, marks original HTLCs spent, then enters durable `Progress::Delivering(Delivery)` for an external recipient. `Delivery.vtxos` are fully signed public native outputs. `deliver_lightning_receive` subsequently posts those exact outputs to the mailbox and records a zero-net forwarding movement plus permanent settled record. These are separate ownership and availability transitions.

Public SDK accessors `lightning_receive_checkpoint`, `lightning_receive_state`, `try_claim_lightning_receive` already exist. `MailboxRpcProxy::post_arkoor_message` can pause actual mailbox posting before upstream while preserving genuine claim/registration. `Db::get_user_vtxo_by_id` supplies actual registration evidence through a read-only domain API. No financial rows or ready flags need insertion.

The authenticated REST receive-status response is insufficient to prove signed recipient ownership: it contains phase/HTLC IDs but not Delivery.vtxos, and explicitly includes `payment_preimage`. Do not log the raw response or Debug/serialize the whole SDK checkpoint. Extract only phase/payment hash, public signed output IDs/policies/amounts and observed durable boundaries. Do not open the permanently absent recipient as an observer or independently open a shared live provider DB without its owner's allocated observation path.

## Source-only focused native prerequisite

Authored only `testing/tests/server/qualification_noah.rs`; root registers its module. Test name:

`qualification_native_forwarding_delivery_survives_provider_reopen`

This is **native SDK forwarding coverage, not stock-Noah or LNURL integration**. It uses one real10,000sat invoice, two actual CLN peers connected to native captaind, a bounded200,000sat configured VTXO pool funded with500,000sat, and genuine provider/recipient SDK wallets. Recipient is stopped/dropped before invoice payment and never reopened. Actual mailbox POST fails before upstream; one original payment settles; provider's durable Delivering outputs must validate, be registered Spendable on the server, equal the recipient address policy/value after advertised receive fee, and have no private key in the provider. Provider spendable/pending forwarded value must remain zero.

The test then drops/reopens only the owned provider DB and verifies exact signed Delivery bytes plus movement ID survive. Releasing the actual mailbox gate must deliver those same bytes, write exactly one successful zero-net forwarding movement to the recipient, and clear the checkpoint. A repeated drive of the settled hash must not post again. Full native case has180second deadline; parked observation has30seconds. It never constructs a fake financial receipt, accesses a live shared wallet, or records a full checkpoint/preimage in public output. No source/compile/runtime PASS is implied; this draft needs independent review and allocated compile/runtime gates.

This deliberately stops before recipient return/expiry. It closes the registered-undelivered interruption prerequisite without pretending that mailbox acknowledgment proves recipient discovery, fallback or stock-Noah HTTP compatibility.

## Remaining authentic lifecycle evidence

| ID | Missing actual evidence | Minimal meaningful oracle |
| --- | --- | --- |
| LNQ-NOAH-01 | Original stock-Noah strict callback invoice | Actual original metadata and amount callback; strict existing verifier passes or preserved compatibility FAIL before dispatch. |
| LNQ-NOAH-02 | Native registered-undelivered provider interruption | Draft above: genuine settled payment, registered recipient-signed outputs, exact durable byte retention and one resumed mailbox delivery; source only so far. |
| LNQ-NOAH-03 | Actual stock backend with recipient offline/provider running | Admit stock pinned Noah+LN-enabled Bark runtime; one original verified invoice/hash; provider receive/claim and actual signed recipient lineage; recipient remains absent. CLN payment alone or backend health does not pass. |
| LNQ-NOAH-04 | Registered recipient outputs expire while mailbox remains unavailable/provider stopped | Cache real signed Delivery paths and recipient registration, then fence provider at declared boundary; canonical payout to recipient's registered script, no provider payout from custody inference, gross=net+actual1000/output fee; independent seed-only recovery/confirmed spend. Use regtest for interruption; public Signet for campaign. |
| LNQ-NOAH-05 | Mailbox accepted but acknowledgement lost | Proxy forwards once then loses reply; provider reopening repeats same signed output IDs; no duplicate recipient principal/history/payout; actual upstream acceptance distinguished from merely attempted post. |

A missing-mailbox recipient fallback native test can follow the new component fixture, with real watchmand funding and actual signed paths. It must not credit the provider for holding a pending Delivery checkpoint. The long public profile's up-to4,320block lifetime means prompt successful receive cannot establish full public expiry; report native ownership/fallback separately from ordinary short-profile lifecycle. No mobile/Android/Noah-client work is needed or authorized.

## Prepared offline parser for the frozen probe

The following is executable observer source, not an executed probe. It accepts paths to the original metadata response body, original callback response body and coordinator-frozen profile JSON. It performs no HTTP/payment. The profile declares `https_origin`, `username`, `lnurl_domain`, `ark_address`, `metadata_request_url`, `callback_request_url`, `amount_msat`, `network`, `observed_unix`, `verifier_path`, `verifier_sha256`, and the admitted `lightning_node_pubkey`. Capture stdout/stderr privately. Freeze the profile and response hashes before running it; changing a failed expectation is a new probe, never a retry of an unknown invoice creation.

```python
import hashlib, json, pathlib, subprocess, sys, urllib.parse

def bounded_json(path):
    raw = pathlib.Path(path).read_bytes()
    assert len(raw) <= 256 * 1024
    return raw, json.loads(raw)

metadata_raw, first = bounded_json(sys.argv[1])
callback_raw, reply = bounded_json(sys.argv[2])
_, profile = bounded_json(sys.argv[3])
amount = profile['amount_msat']
assert type(amount) is int and amount >= 1000 and amount % 1000 == 0
assert profile['network'] == 'signet'
origin = urllib.parse.urlsplit(profile['https_origin'])
assert origin.scheme == 'https' and origin.netloc and not origin.path
base = profile['https_origin'] + '/.well-known/lnurlp/' + profile['username']
assert profile['metadata_request_url'] == base
callback_url = urllib.parse.urlsplit(profile['callback_request_url'])
assert urllib.parse.urlunsplit(callback_url._replace(query='')) == base
assert urllib.parse.parse_qs(callback_url.query, strict_parsing=True) == {'amount': [str(amount)]}
assert first['callback'] == base and first['tag'] == 'payRequest'
assert type(first['minSendable']) is int and type(first['maxSendable']) is int
assert first['minSendable'] <= amount <= first['maxSendable']
metadata = first['metadata']
assert isinstance(metadata, str) and len(metadata.encode()) <= 65536
pairs = json.loads(metadata)
assert isinstance(pairs, list) and all(isinstance(p, list) and len(p) == 2
    and all(isinstance(v, str) for v in p) for p in pairs)
assert [p for p in pairs if p[0] == 'text/identifier'] == [
    ['text/identifier', profile['username'] + '@' + profile['lnurl_domain']]]
invoice = reply['pr']
assert isinstance(invoice, str) and 0 < len(invoice.encode()) <= 65536
assert reply.get('ark') in (None, profile['ark_address'])
verifier = pathlib.Path(profile['verifier_path'])
assert not verifier.is_symlink()
assert hashlib.sha256(verifier.read_bytes()).hexdigest() == profile['verifier_sha256']
policy = {'invoice': invoice, 'amount_msat': amount, 'metadata': metadata,
    'network': profile['network'], 'now': profile['observed_unix']}
result = subprocess.run([str(verifier)], input=json.dumps(policy).encode(),
    capture_output=True, timeout=10, check=False)
if result.returncode:
    sys.stderr.buffer.write(result.stderr)
    raise SystemExit(result.returncode)
proof = json.loads(result.stdout)
assert proof['signature_checked'] is True and proof['amount_msat'] == amount
assert proof['network'] == profile['network']
assert proof['node_pubkey'] == profile['lightning_node_pubkey']
assert proof['invoice_sha256'] == hashlib.sha256(invoice.encode()).hexdigest()
assert proof['metadata_sha256'] == hashlib.sha256(metadata.encode()).hexdigest()
print(json.dumps({'status': 'VERIFIED_ORIGINAL_INVOICE', **proof,
    'metadata_response_sha256': hashlib.sha256(metadata_raw).hexdigest(),
    'callback_response_sha256': hashlib.sha256(callback_raw).hexdigest()}))
```

This parser cannot itself prove TLS verification, nonredirected transport, invoice-creation uniqueness or declared payer capacity: those belong to the coordinator's original frozen HTTP/journal receipts. It must not be used to manufacture them after a timeout. The real controller's existing NativeInvoiceVerifier is preferred when the allocated campaign already uses it; the standalone parser is the small offline alternative, not another orchestration framework.

Peer review corrections to the native draft: successful delivery capture now includes the exact blinded recipient mailbox ID as well as signed bytes. The fixture explicitly keeps the repository's existing `ln_receive_anti_dos_required=false` profile; the SDK's missing proof is caught and sent asNone, accepted by this configured server. This does not qualify a token/proof-required server. Its one successful mailbox post follows pre-post outage only; lost-ack repost semantics remain LNQ-NOAH-05.
