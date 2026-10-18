# Proposed unexecuted ordinary barkd recovery cases

This is a source-only proposal to root, with no runtime allocation. Exact binary for all three is `ark-testing::barkd`:

1. `recovery::recovered_wallet_skips_exited_vtxo`: real background unilateral-exit progression plus fresh same-seed recovery must retain exactly the original ID as **Exited**. Current recovery independently checks confirmation of the actual VTXO-point transaction. This does not rely on treating historical round/conditional inputs as Spent. Existing helper is bounded120seconds under a future default180 gate. No explicit final claim/drain, Core6 or full-fee conservation oracle is present.
2. `recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit`: xpub recovery disabled, receive50k after60 genuine address derivations, then prove gap50 misses and gap100 rediscovers the exact same ID in separate fresh wallets.
3. `recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub`: normal test xpub scan crosses gap50, recovers that exact current ID, and does not hand out the received address again. Native test xpub limit100000 may add CPU cost.

The gap pair proves discovery-window behavior, not independent spendability/payment: current source asserts complete-list IDs/counts and one address inequality without asserting recovered Spendable state or all balances. Original actors remain alive in all three cases; these are separate fresh same-seed databases, not absent-actor or physical-backup tests. Fee-shape changes do not alter their fixed expected net amounts because they contain no old fixed net-fee assertions. Runtime180 feasibility remains unexecuted.

All three occur as unignored entries in the existing saved batch45 case01 listing. None has a PASS/FAIL coverage row or exact candidate-literal occurrence in curated batch01–45 receipts. Source/archive and helper/product recovery files match frozen v10 bytes and canonical source. The JSON binds exact function/body/helper/full-file/source/listing/coverage/receipt hashes; old metadata is evidence only and is not reused as a future allocation.

Excluded are executed names/retries, the explicitly held unsafe server-word original, old fixed991443/699443/999443 recovery payments, and round/conditional-history oracles needing a separate grounded policy adapter. A valid same-anchor ordinary Pubkey discharge can establish Spent; an unsupported historical round or conditional source cannot be silently accepted by broadening the shared helper.

Future candidate/source/archive/ELF/metadata/pins and allocation fields remain NULL. Required future default180/retry0/serial1/first-failure stop/global2, exact body/listing/version/resource gates remain. No CLN is required. No source/runtime/global/frozen-payload mutation or execution/PASS/FULL ML credit.
