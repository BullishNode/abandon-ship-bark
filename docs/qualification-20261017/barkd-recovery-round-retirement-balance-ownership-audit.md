# Retired round ancestor: balance and exit ownership audit

The current 0cf adaptation has an oracle gap: it checks the current spendable cut and zero `.spendable` after payment, but does not assert every other positive balance bucket or exit intent. No actual double balance or old-ancestor exit was observed.

Immutable case10 SQLite shows exact old546a is simultaneously unproven, awaiting settlement and quarantined; it has no entitlement, ordinary proof, action checkpoint or exit entry. The original and recovered current spendable cuts match649392. Captured e00 balance code removes unproven IDs before all bucket accumulation; its protection ownership guard refuses this historical ID without an authenticated current customer claim. These are source-derived expectations, not a measured full-balance response or future act-by execution.

The public per-VTXO awaiting flag can correctly be true for this retained history. It does not mean the monetary awaiting bucket includes300k. No all-bucket numeric response was found by the bounded scan of retained recovered logs.

The minimal prospective fixture improvement is full ten-field balance assertions before and after the real payment, plus the existing public all-exit-status query requiring no current exit intent. Preserve exact known-ancestor body/Locked disposition, strict shared helper and valid Arkoor Spent control, and all real current-cut/recipient/Spent-source assertions. A current empty exit list alone cannot prove a future deadline run; no REST protection-report API exists in the captured source. No source edit, actor, retry or new runtime occurred.
