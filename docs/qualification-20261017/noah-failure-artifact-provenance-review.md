# Noah 8e68 failure-artifact provenance

**Resolved: the retained actual failure is one strict LNURL invoice-compatibility rejection, before payment.** The immutable simulator object is `a916b7ae1918c639253bba88ba95676e97d81b0d:docs/evidence/noah-actual-invoice-rejection-20261017.json`. It is readable from `/home/francis/bark-simulator-fresh-integration`, although absent from that checkout's current working tree. Exact public bytes are preserved as `noah-actual-invoice-rejection-20261017-public-copy.json`, SHA256 `dcc4410247d48589b0ba1bd6f7f13c0725d649f663fccaad25b5373110544ab6`. The original Git object and failed attempt remain unchanged.

At **2026-10-17 16:11:48.072315 UTC**, public Signet height **326772**, one callback requested **5,000,000 msat / 5,000 sat** without Ark negotiation. Stock Noah returned **HTTP 200**, while strict invoice verifier `e97c422b…` exited **1** with the public error `LNURL metadata hash missing; plain description is incompatible`. Verification was not relaxed and no invoice was regenerated. The original exact response/invoice/policy/output remain private, bound by public hashes. This reviewer did not decode private invoice bytes or open full native logs.

| Historical component | Exact pin from the public packet |
|---|---|
| Stock Noah source | `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0` |
| Noah backend ELF | `87b2c181e5ba6e57bdb9ac715d2cb56602f2edc6f621c42ef7c6fd397489d187` |
| Fresh captaind ELF | `32ea16685558bf44e55c110d3574c3bd9e93285e8ba27115b57fb3f1ae98aaf4` |
| Captured product source | dirty `fb85d9a9c3b31b3eab1eb2113d2caec89c7f10dd`; manifest `0ff7819dc89062aef8c6ce43b81d99c851fd813ef0ac75f0d462ef457deb6b69` |
| Published equivalent code recorded in receipt | `f809bc2410b40648823caea671fe52dd0c86f11b` |
| Native provider source / Barkd ELF | `7055d1fce858f3112aff671d0b2a8a454d6a2214` / `2c53f1a5bdd974f74e1c1c45b497e44d7e2abfd0f2a9bfa426e06a4eccc73771` |
| Strict verifier ELF | `e97c422b5ee894b3d5486ff139039d46107cc0e723adb9dea5a94ccd501798b1` |
| Verifier source | simulator `3da8a0be1324e0c4351605f3f6c72fd0dc6a561f`, `integration/noah-public-proof` |

The fresh profile had a distinct server public key, lifetime 4,320 blocks, exit delta 144, three board confirmations, empty pool targets, no watchman and zero wallet funding. The recipient was offline. **No Lightning payment, funded swap, recipient claim/delivery, fallback or seed recovery was demonstrated.** This is an invoice-contract failure, not a failed payment or loss-of-principal result. These historical pins do not qualify the later source7041, source87e or current product candidate.

The callback public hashes are response `fd58daf7946c03e335e6a8637ab6d5d8e67c84067b4119f269763db0d0bb6264`, invoice `8ef568f30a6c2a4a663a832186b6821f8c19709a20dc9a49245fde1922578917` and exact metadata UTF-8 `947f229a83bc7b8748c4dc770969be349e384ea6cef7c34648c5130802462f8f`. The linked public metadata receipt supplies the exact metadata string; recomputing its SHA256 matches the actual callback's recorded digest. The requested amount is within its public bounds of 1,000 to 1,000,000,000 msat. This is a cross-check of public receipt consistency, not an independent private invoice parse.

The two original locators now have their correct roles. `noah-invoice-proof-artifact-20261017` is the exact e97c standalone validator later used in the rejection, not the failure packet itself. All six artifact hashes and five source-file hashes match; the safe protocol summary confirms five offline tests passed. Its original missing-arrayvec cache failure and the missing older `f96` binary remain explicitly preserved. The linked public verifier receipt confirms its VM negative smoke and measured provenance. `noah-native-preparation-7055.json` is earlier zero-funded setup, with no invoice/payment attempt; it cannot by itself establish the later callback failure.

The failure was located through historical `STATUS-before-20261017T171811Z.json`, hash `facc8f6c6f25db03debbbd1b088146fff1312b6af149e634973df627c093bf2d`, whose Noah section points to commit a916b7ae. Later/current STATUS omits that section; omission is not erasure of the original measured failure. The receipt's fresh native profile and exact hashes are authoritative for this attempt.

Safe public copies of the original failure, verifier and metadata receipts are named in `noah-failure-artifact-provenance-review.json`, with exact hashes and Git-object paths. No SSH, VM/service call, callback request, payment, Cargo/build, source edit or original-artifact mutation was performed. Private identity, TLS, admin/token, mnemonic/private-key and raw wallet files were excluded. Backend stock-source and SDK invoice validation reviews remain separate owner tasks; no verification bypass or new financial workflow is proposed here.
