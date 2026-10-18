# Independent money-safety fixtures

Both exact test bodies passed once after coordinator integration into the dirty V11c capture. These are bounded component results. Production readiness and full ML acceptance remain unestablished.

| Scenario | Test | Result and bounded assertion |
| --- | --- | --- |
| ML049 delivery component | `qualification_forwarded_ack::forwarded_receive_ack_loss_replays_identical_outputs_and_credits_recipient_once` | PASS, 22.588 seconds. Two real accepted mailbox posts and one lost reply preserve the same 10,000-sat output, one credit and one movement. One original payer record is COMPLETE. |
| ML186/ML187 alternate recovery | `fallback_external::fallback_external_bip84_seed_restore_spends_actual_payout_while_history_unavailable` | PASS, 25.923 seconds. Fresh stores and the two original independent seeds recover the actual 99,000-sat BIP84 payout while history RPC remains unavailable. A real recovery transaction delivers 98,781 sat and pays 219 sat to miners. |

The fixtures use genuine native payments and signed outputs. The faults alter transport responses only. They do not fabricate money, signatures, recipient ownership or confirmations.

## Source and execution

The source branch starts at `4fa8997ba21158f20326c19f68efba5f42af1f3c`. Only two test files and this intake document differ from that base. The previous two external-wallet test bodies remain byte-identical. This branch leaves the new server module unregistered. The coordinator integrated the accepted patch and added its binding in `testing/tests/server/mod.rs`. Do not merge this old source base over newer product work.

Actual tests used the coordinator's exact dirty capture at HEAD 0496e057fe8cedde99c9b85edab67035633a8f25:

- Source manifest: `cdedd62efcf1e4f131f8378017cae5facd729d36995419a4ff2e36d0456bf635`.
- Source archive: `acc3dfec248801a5476085cc3f917142bb7e22df1b090df913ba92c8c8baaf58`.
- ML049 file: `456f7b8c999fc4c6692e13671392d0be8800f0c00789af4798983aaee858ddee`.
- Recovery file: `90339cdd2e46df18f53d591f907bee4f7a0a1d97b7242564eb25972baa3539e9`.
- Native run IDs: `59b44ec7-50ad-4e2e-a6d6-2bd7a469b055` and `1584f140-2495-4079-b4c2-de5e7b9f99b2`.

The coordinator allocated one serial two-case parent. Both exact unignored singleton selections passed. Execution used the existing `just int-binaries` protocol, default 180 seconds per case, retries 0 and first failure stop. Both tests passed on their first attempt. All 17 runtime pins and 20 admission control/receipt hashes remained unchanged afterward. No owned actors remained. No cleanup or signals occurred. The initial postflight reader used the wrong plugin-line prefix; its failure remains retained. A separate reader recognized the actual required `important-plugin=` lines. No native test was retried or changed.

For ML049, `BARK_DOUBLE_DRIVE_ACTIONS` and `LIGHTNINGD_DOCKER_IMAGE` were completely absent after the approved cached Nix environment was sourced. False, zero and empty strings do not meet the absence requirement. The actual native CLN 26.06.6 executable and both approved plugins were pinned. The reentrant recipe was not used.

## Independent financial evidence and limits

The retained recovery graph independently conserves 200,000 sat of actual backing and sponsor inputs = 197,500 terminal sat + 2,500 miner-fee sat. Parent and child have 12 confirmations. Recovery has 6 confirmations. The complete canonical interval contains one output to the payout script, including spent history, and no reused non-coinbase input.

The mailbox audit independently reconciles public phase, quote and original payment records. Signed-request equality, materialization, actual mailbox effects and underlying Lightning state remain native assertions. The chain audit does not independently establish recipient identity, seed provenance, private-key custody or consensus/signature validity.

ML049 does not qualify fallback, seed recovery or Noah. The recovery fixture does not create actual oversized history, revision churn, Lightning, backup or reorg. Neither result closes a full ML scenario. No release admission follows.

The immutable final receipt is `native-two-fixtures-results-001.json`, SHA256 `71b9583d24fded71fc1e41b73545f34a3107e627212dc6e7cd7871724504abbc`. Append-only local reports and exact artifact references are under `/home/francis/bark-integration-2026-10-01/independent-money-tests-20261017/`. Raw private runtime data, credentials, seeds and preimages are excluded from this branch. Coordinator intake and curation remain separate.
