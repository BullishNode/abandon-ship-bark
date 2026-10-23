# Fallback payout candidate

Use fresh server and client stores for this fork schema. Complete stores from
the same schema can reopen without resetting registrations or unpaid obligations.
Earlier experimental fork database formats are not converted. Preserve earlier
stores and backups.

Bitcoin-address and output-script history queries now read the complete wallet
history. They preserve exact matching and movement order. Work and memory use
grow with stored history. A decode or projection error in another movement can
fail an address query. Other payment methods keep indexed queries.

Use the release-pinned complete Core Lightning runtime, `v26.06.6-abandon6`, with
its matching subdaemons and plugins. The ordinary upstream runtime does not
provide the required on-chain HTLC failure boundary. Internal Lightning sender
refunds require all necessary grant closures to reach the configured depth.
That depth defaults to six confirmations and cannot be lower. The protocol
supports reorgs that remove one through five blocks and assumes none removes six
or more. This assumption does not make bitcoin finality absolute. Qualification
of the supported reorg boundaries remains incomplete.

Read the [operator manual](docs/operator.md) before installation or recovery.
Financial and release qualification are incomplete. This candidate is not
production ready. It is for testing and is not approved for bitcoin mainnet.

## Current limits

- Ambiguous external Lightning outcomes can remain pending. Supervised recovery
  of a request that never reached Lightning requires intact original history,
  complete producer and queue shutdown, and retained incident records. Outcomes
  must match the original payment request and its inputs. A timeout, empty
  history or shared payment hash does not prove failure. Missing history or an
  unproven shutdown can prevent recovery.
- SDK key and address issuance requires authenticated fallback registration.
  Supplied-address forwarding does not verify the recipient's registration.
  Unknown owners and registered owners with incomplete attribution remain held
  and can delay shared-tree payouts. Never assign their bitcoin to the provider
  or operator. Restore must retain all accepted registrations and financial
  commitments.
- Payouts require confirmed usable watchman fee inputs. The operator miner
  subsidy has no hard cap.
- Required financial, recovery, history and full regression checks remain
  incomplete. Earlier results apply to their original source and programs.
  The remaining checks and final independent review are required for production
  qualification.
- Private Signet produced an ordinary fallback payout, a sender refund and two
  actual BIP84 seed-restored spends. The lifecycle remains partial. A recovery
  command sequence continued after a failed fee-quote precheck. The no-refund
  observation at five confirmations did not isolate the confirmation rule from
  fee and readiness delays. Native BIP86 seed recovery demonstrated discovery,
  not an actual spend.
- Final GNU release builds, embedded and served wallet assets, browser behavior,
  operator recovery drills and the measured operating limits remain unqualified.
  A successful build or a bounded payment scenario is insufficient.
- The required final upstream rebase remains unfinished. Review its conflicts
  and retained protections, then rebuild and verify affected programs and
  dependencies. Retain earlier evidence only for demonstrated unchanged paths.
