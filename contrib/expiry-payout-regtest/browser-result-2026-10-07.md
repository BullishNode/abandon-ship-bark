# Native task browser checkpoint

Local regtest only. PASS20261008T004650-6ab9ce0d using client36d1a07c3503690857fab499483f13432886ebc8, bark3a8f9694ad41c08be4d2ca634d380b99dbb1d427, client image sha256:75ab091c616f21947d7330566e1bdc6c167960944e9ffda1dc7e64df26d0a602. captaind sourceb904859cda0cd451a6e635c1e6f4827708d6a7ae; binarySHA25605930d221bd5af4b753646e4e7d9967517432bed804176131172af7eb9c30266.

Real Hono API, Nginx, fresh barkd wallet and headless Chromium; no mocked wallet endpoints. The same browser assertions were used for A and D:

- Actual import form restores the scenario seed with birthday height through real Hono API and pinned barkd.
- Actual expiry payout discovered from seed; exact net/fee visible; mobile and desktop have no horizontal overflow.
- Receipt outage reports unknown fee and preserves payout amount/spend action.
- Privacy hides payout amount and deducted fee.
- Browser broadcast spends exact payout once; Core verifies separate transfer mining fee.
- Duplicate spend rejected; confirmed balance reconciles without double count; history separates fees and honors privacy.

Gross 119670sat, net 119208sat, expiry fee 462sat. Payout `b5b43c5040baa0643c228bd432b88c7d5c895b2c76cd426a8213134141071ee9:0`. Wallet transfer `519ff0e9f8ec5cedaac91226e1b0b4eb4a27adfc7255a1b186b9967dc5b85fef` receives118875sat after its separate333sat fee. Total balance1999053 ->1998720sat. Browser exceptions0. Core verifies the exact spend and mining fee; repeat spend is rejected.

Local raw evidence/scripts/screenshots: expiry-task-evidence/client/ under the owner's integration notes. No seed is in this report. This is not signet evidence and does not cover every normal wallet flow, native bindings, or complete accessibility. Final image and signet qualification remain open.
