# Independent parent source review

Reviewed exact owned SDK source SHA256 `3fef86397a648c4b11ef92f2f0c252b870e9ec766cc1ac14ff96075347964c88` and target-only patch `4ae8f31745b3f84a383493a957c3f7045c4fe4d12e59d6501ad5517e59bfc72a`. The complete replacement was read. Canonical `git apply --check` passed; no patch was applied. Current typed refusal variant, stable error code, public refresh/pending-send APIs and builder watchman scheduling behavior were inspected. The independent legacy review additionally covers native schema and API details.

No concrete remaining source blocker was found. The refused invoice is genuine, and refusal requires retained signed principal, zero effective failed history, no pending checkpoint, no server attempt/dispatch, no actual CLN pay and no received HTLC. The success counterpart uses an actual refresh, signed replacement anchored with six confirmations, a distinct invoice and real Accepted-to-Paid settlement with CLN completion/preimage and exact native balance checks. This avoids crediting a refusal-only mock or an artificial expiry update as successful recovery.

Manual client synchronization and 900-second watchman scheduling are declared fixture conditions. They isolate admission before an actual payout decision and do not prove default-daemon behavior. This case also does not prove fallback payout, Lightning backup, first-attempt races, capacity or a full money-loss scenario. The original incompatible function remains held and unrun; it is not relabeled as a failed execution.

ACK for source readiness only. UNCOMPILED / NOT RUN. No Cargo, module registration, canonical edit, native execution, retry or PASS credit was performed by this review.
