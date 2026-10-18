# Owner ruling: maximum reorg depth

Date: 2026-10-18. Given by the owner in the Claude Code session that ran the abandon-ship simplification
research and the option B proof of concept (session francis-70).

> "I don't think reorgs deeper than 3 will ever happen."
> (earlier in the same session: "let's assume that reorgs deeper than 3 blocks never happen")

## Ruling
Reorgs deeper than 3 blocks are out of scope for the fallback design's safety guarantees.

## Consequences
- Option B's "no double payment" guarantee holds for any refund depth N >= 4. The code must refuse N < 4;
  recommended default N = 6 for margin.
- This is stricter than plan C10 (`abandon-ship-final-plan-2026-10-16.md:418-422`), which states that 100 blocks
  is an operational assumption and not finality, and requires rebuild after deeper reorgs. C10's text should be
  amended if this ruling is adopted into the plan.
- Bitcoin does not enforce this limit. A reorg deeper than 3 blocks that removes a grant reclaim after the
  sender's refund was signed lets the operator pay both sender and receiver (measured: test
  `expiry_internal_reclaim_reorg_after_refund_pays_once`, N = 1, 5-block reorg, run s1h). Customers are not harmed;
  the loss is the operator's.
