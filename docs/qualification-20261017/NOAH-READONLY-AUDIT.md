# Stock Noah offline LNURL: read-only qualification audit

The retained stock-Noah callback failed the accepted campaign's strict metadata-binding check before payment. The failure is genuine, but its scope is invoice acceptance: no funded swap, delivery, fallback or seed recovery was demonstrated. This audit recovered and cross-checked existing evidence; it did not repeat the callback or change the acceptance rule.

## Observed attempt and provenance

At 2026-10-17 16:11:48.072315 UTC, public Signet height 326772, one callback requested 5,000 sat from stock Noah `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`. HTTP returned 200; independent verifier `e97c422b5ee894b3d5486ff139039d46107cc0e723adb9dea5a94ccd501798b1` exited 1 for a missing invoice description hash. No invoice regeneration or payment followed. The profile had zero funding and no running watchman.

The immutable original is simulator Git object `a916b7ae1918c639253bba88ba95676e97d81b0d:docs/evidence/noah-actual-invoice-rejection-20261017.json` in `/home/francis/bark-simulator-fresh-integration`. Its byte-identical safe copy is [noah-actual-invoice-rejection-20261017-public-copy.json](noah-actual-invoice-rejection-20261017-public-copy.json), SHA256 `dcc4410247d48589b0ba1bd6f7f13c0725d649f663fccaad25b5373110544ab6`. Public metadata receipts independently reproduce the exact 141-byte metadata string's digest, `947f229a83bc7b8748c4dc770969be349e384ea6cef7c34648c5130802462f8f`. Invoice bytes were not independently decoded in this audit; their retained public digest is `8ef568f30a6c2a4a663a832186b6821f8c19709a20dc9a49245fde1922578917`.

This historical pairing used dirty captaind/server source `fb85d9a9c3b31b3eab1eb2113d2caec89c7f10dd`, source-manifest digest `0ff7819dc89062aef8c6ce43b81d99c851fd813ef0ac75f0d462ef457deb6b69`, and native provider revision `7055d1fce858f3112aff671d0b2a8a454d6a2214`. It does not qualify the later v5/v6 executables. Later preparation records reporting no attempted payment do not erase the earlier callback rejection.

## Source explanation and attribution

Stock Noah's `server/src/routes/public_api_v0.rs` advertises serialized metadata containing both identifier and plain text, but its callback supplies only the human-readable memo `Paying satoshis to {lightning_address}`. `server/src/barkd_client.rs` accepts only an optional description string. The provider path carries that string through REST, SDK and RPC to the hold plugin's Memo variant. Neither the exact advertised metadata bytes nor their digest reaches the issuer. The hold protocol already has a distinct Hash variant.

The reviewed memo-only receive API and the SDK's warning-only LNURL description checks are inherited from upstream `bfbe6b7c3088a09dd4e81c4ac24cdc763a627a8c`; this audit does not attribute their introduction to the expiry fork. Existing memo-survival tests constrain compatible changes. A direct-provider invoice test bypassing Noah discovery/callback cannot establish stock-Noah LNURL compatibility.

## Protocol scope and legitimate next action

Current [LUD-06](https://github.com/lnurl/luds/blob/luds/06.md) no longer mandates metadata description-hash verification; the [upstream removal](https://github.com/lnurl/luds/commit/30339780e8f709b839b639c65a50ff99f8a0abcb) explicitly removed that requirement. [BOLT 11](https://github.com/lightning/bolts/blob/master/11-payment-encoding.md#tagged-fields) permits exactly one description or description-hash field. Missing h alone therefore does not establish a malformed BOLT 11 invoice or a violation of current LUD-06. This finding does not certify other Noah behavior.

The accepted October 12 campaign separately requires strict metadata binding, and the coordinator explicitly retained that rule. The original attempt remains FAIL under that contract. Its original wording about a mandatory hash must be read in that campaign scope; the historical receipt remains unchanged.

If that strict integration target is retained, an approved compatible change must carry an explicit, mutually exclusive 32-byte digest from the caller across REST/SDK/RPC into hold's Hash variant. It must commit to the exact advertised metadata bytes, reject invalid or conflicting inputs before invoice creation, and preserve existing memo behavior. Hashing the memo alone is insufficient. Adding provider capability alone cannot repair unchanged stock Noah, whose caller does not send the digest. An upstream-compatible caller change and a separately admitted strict callback proof would be required. No Noah fork, mobile work, provider implementation or policy relaxation was performed or authorized by this audit.

The remaining financial qualification still needs a genuine paid native swap to an offline recipient, the rightful registered claim/delivery, real expiry and fallback, original-seed confirmed recovery, and a genuine failed-send/refundable path. Invoice-only evidence supplies none of those results and earns no full legacy money-loss credit.

## Review evidence

The three independent reviews and additive corrections are retained in this directory:

- [Backend source review](stock-noah-lnurl-metadata-source-review.json), SHA256 `fa3b42f1c10b56751822f481895b6092bb2489203e10ce46ad89cd7ad26b1150`.
- [Client source provenance](noah-lnurl-client-audit-provenance.json), SHA256 `d64181d7de41d181cbebbb722398ca8577914156ccfca30a05bdf965430bbdff`, and [observed addendum](noah-lnurl-client-observed-addendum.json), SHA256 `91d750d668280448c47e88960b2c9399a4f2c813b4c762f1a3d93923932be5fe`.
- [Original failure provenance](noah-failure-artifact-provenance-review.json), SHA256 `95ecc9e06b56c59fc0df8101167e8d9b22ae74ff8ea41143c227fa40b2e137fd`.
- [Protocol scope addendum](noah-lnurl-protocol-scope-addendum.md), SHA256 `7f3b54f8c38f495c2728bcd243e501a6f181580e3ea29c18f4c44bb46162612c`.

The observed addenda supersede the initial source reviews' pending-artifact limitation only. Original failures and frozen reports remain intact. This review used local public receipts, immutable source and primary protocol documents. It made no VM/service calls, callbacks, payments, builds or runtime changes, and opened no private identities, TLS credentials, tokens, seeds or wallet databases. Native regression counts and original money-loss acceptance maps are unchanged by this audit.
