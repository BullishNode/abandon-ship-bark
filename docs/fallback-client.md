# Fresh fallback client

The application supplies its permanent fallback address through `OpenWalletArgs::fallback_spk`. The SDK authenticates the signed mailbox, coin-account xpub, network and exact address binding. It accepts standard Bitcoin address scripts, including an address controlled by a separately seeded application wallet; it does not derive that address from the Ark seed or require a concrete onchain wallet type.

The native application explicitly selects external index zero from its attached BDK wallet and reserves it from normal funding-address issuance. Upstream BIP86 is the default descriptor. The `fallback-test-bip84` feature on `bark-wallet`, `bark-cli` and `ark-testing` selects BIP84 for the test application. A build must use the descriptor that created its wallet database; switching features is not a database migration.

A cached record is authenticated before reuse. Supplying a different explicit address is an error. A seed-only or offline read-only open can reuse an authenticated cached binding without an attached onchain wallet. A new wallet without an application-selected address stays unregistered and cannot issue or spend coin keys. Registration retries adopt only an exactly matching authenticated server record.

Native barkd exposes `GET /api/v1/wallet/fallback` through its existing authenticated wallet router. It reports whether a registration exists and its immutable payout address. The endpoint does not reveal private keys or manufacture a registration from a status read.

## Validation

- Mandatory `just checks`: passed (including WASM and full native workspace checking). The first attempt caught a missing `ScriptBuf` import; the corrected source passed.
- Full `just unit`: 757 passed, one pre-existing explicitly ignored test skipped.
- `ASSUME_BUILT=1 just int "--features fallback-test-bip84 --test bark-sdk fallback_external"`: one passed in 18.623 seconds, nextest run `6a9cf49c-4abe-4042-b3bb-14c79d92d239`. It uses different Ark and onchain seeds, new databases on restore, actual PostgreSQL and Bitcoin Core, and the real public `OnchainWalletTrait` boundary. The client SDK is this milestone's feature-enabled source. Its unchanged registration wire was tested against the existing W2 server binary, source equivalent to `97ae445aeea4dcb966a6084c2393cf8eed3470a3`, whose version metadata is `901617a4e-w2-working` and SHA256 is `fc7723588735786b47de21a931fe693bfc6ecc1f439a7a5e5083c33fdfea2ca2`. This deliberately does not assert a rebuilt client CLI or new backend endpoint was exercised.
- The native barkd route test needs a rebuilt integrated barkd. It has not yet run against this route.

An ordinary onchain deposit in the adapter fixture proves wallet attachment, registration, restore and receipt recovery. It does not prove an actual fallback payout. Actual payout, expiry reconciliation, authenticated payout history and web manual acceptance remain later milestones.
