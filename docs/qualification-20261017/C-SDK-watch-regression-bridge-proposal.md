# Captured C SDK regression bridge proposal

SOURCE ONLY — no allocation, fresh listing, readiness, or native execution. The three existing names retain their batch44 PASS only on old source d0fb8640; they have no C execution result. Repeating them would add zero new unique scenarios.

| Order | Exact binary/name | Old batch44 attempt | C source | CLN |
|---|---|---|---|---|
| 1 | `ark-testing::bark-sdk` / `lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds` | PASS 39.847s; `a5bc02b0-5357-4440-98bb-59f4cb4dd5b2` | function `d97faa40b0c5cbd35001fd89bb2183b10360a463aae8efe1de25e785b5fc5bfb` | native channel + hold |
| 2 | `ark-testing::bark-sdk` / `exit::watchman_sweeps_funding_tx_during_exit` | PASS 49.248s; `f41b4b18-b14b-422d-ba54-513ec19240dc` | function `aa77742686167022fe1ea479184810357f44f15a098c3fb8e48fa22b2d0bea80` | none |
| 3 | `ark-testing::bark-sdk` / `exit::watchman_sweeps_vtxo_chain_during_exit` | PASS 57.051s; `57fbc695-a764-4c4f-9684-432e5dac01c2` | function `6ca76e17a543d976bbe9a31bdd776e6658ea44cf57c01619021c130c68e17ecb` | none |

Both complete test files and ten common helper/module files match old44→C byte for byte. Exact function/body and helper hashes are in the JSON. The saved P07 listing has all three unignored, with string-filter mismatches because P07 alone was selected; this is compiled-name evidence, not a new selection or run.

The first case proves typed expired-new-Lightning refusal with no dispatch, real refresh, then a distinct real CLN hold-payment completion. It does not prove unilateral fallback payout. The watchman pair proves canonical Core6 registered customer payouts and exit/history reconciliation, including a genuine partial exit chain. It does not independently close all fee-child/sponsor mining-fee and custody accounting. Explicit manual scheduling and split factor1 remain fixture conditions.

This is a bounded product-regression bridge for C paired claim-journal changes; unchanged test bodies do not force or qualify the new root reorg/fairness journal boundaries. No held unsafe name, old fee literal case, or reentrancy fixture is proposed.

Future admission requires root allocation and parent sole launch after source/ELF/metadata/17-pin and exact-singleton gates, native CLN26.06.6/grpc/hold for the first case, fresh private contexts and resource guards. Defaults: 180 seconds per case, retries0, serial1, first failure stop, global maximum2 cohorts. All new allocation/readiness/admission fields remain null.

Proposal JSON SHA256: `b20b693e35a803d030eed508a185766e3f12740ffe30aab17e32ccadab28d017`. Existing artifact proof: `ac69ffbf60f1cb15a854f1a48eb295f1f513394c4e0600fcef9466ca9cbb074b`. Saved listing: `8071f556c8a4ba05878f19f673faf901fd57f2742cf194c53dc9fc3f4c297486`.

No source, control, global report, build, Cargo, fresh listing, preflight, resource probe, actor, SQL or native action was performed.
