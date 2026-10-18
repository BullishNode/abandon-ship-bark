Independent evidence ACK: **no concrete publication blocker found** within the reviewed seventeenth-checkpoint cutoff. This is a new read-only review; no source, runtime, Cargo, rolling report or historical receipt changed.

All six literal names, expected binaries, run IDs, PASS states, exit0/resource-error-none fields and native durations agree across the initial528a52d3 receipt, formal96e04e64 postflight, current six coverage rows and retained safe result JSONs. Log files were rehashed without inspecting or exporting private log content; their hashes match receipt/postflight.

| Coverage ID | Binary | Actual native run ID | Seconds |
| --- | --- | --- | ---: |
| FT-SERVER-014 | `ark-testing::server` | `51978c4f-4b4c-4f3d-bdbc-3f6e55d49db3` | 34.690 |
| FT-SERVER-015 | `ark-testing::server` | `5838a49e-d1f0-451f-9f82-c27334bffe6f` | 53.453 |
| FT-SERVER-016 | `ark-testing::server` | `b12b96fd-87f1-445d-ba71-9d2f3295113c` | 52.846 |
| FT-SDK-055 | `ark-testing::bark-sdk` | `a5bc02b0-5357-4440-98bb-59f4cb4dd5b2` | 39.847 |
| FT-SDK-056 | `ark-testing::bark-sdk` | `f41b4b18-b14b-422d-ba54-513ec19240dc` | 49.248 |
| FT-SDK-057 | `ark-testing::bark-sdk` | `57fbc695-a764-4c4f-9684-432e5dac01c2` | 57.051 |

Sum: **287.135 native seconds**, six distinct actual executions, no retry. The six literal binary/test pairs are absent from the prior447 rows. Coverage has453 LF rows; the entire header-plus447-row byte prefix hashes to0b7387360a4fbe3cd1887a92f1a2114aa27065b242ccd92f74ded8e9c1ab1d0e and the appended six rows to750ae19671552436c43e7689ba0357ec822448d2684953a2891c65cccc451e0c. The checkpoint’s236→242 executions,221→227 PASS, unchanged15 FAIL,221→227 unique pairs,209→215 invocations and224→230 native plus12 components agree with the preserved sixteenth baseline. This verifies the six-case increment; it does not re-audit all earlier236 executions.

The tested source remains v9b d0fb/f72/fcba/fbc752 with dirty4fa8997b provenance. Corrected SDK bodyd97 and failed-swap fullfile237d/helper ebae are explicitly distinguished from historical eb82/55bb; unchanged wrapper0fe44 alone is not treated as helper provenance. Postflight records17 pins,16 frozen controls, six absent groups/SIDs and scoped/plugin actors. Those are retained postflight observations, not a new live-namespace check in this review.

The conflict audit’s exact declared cut is700,000 =199,000 customer +298,474 ordinary customer change +198,187 other unspent leaves/unused sponsor +4,339 actual miner fees. The actual canonical parent/child are at seven confirmations; one merged payer is an observed C9 outcome, not a policy requiring that payer. Independent Core values/canonicality/duplicate exclusion are kept separate from native owner, signature and custody assertions.

ROOTJ01 is a separate lane and contributes **zero** executions or rows here. Its separately reported680,000 final-package cut is198,000 customer +182,000 other parent leaves +293,066 child change +6,934 actual miner fees; it includes200,000 source +180,000 receive grants +300,000 fee sponsors. It is not the whole original customer/controller funding budget. Historical ROOTJ01 PGID/SID remains unknown under the explicit scoped-absence-only disposition. The final8cf26129 receipt binds unchanged eight Core/safe-result input hashes and post-read scoped absence; it grants no continuous-absence or global-custody claim.

The other five cases are correctly described as bounded native-assertion evidence, without additional independent offline financial audits. Default factor2/default-daemon behavior, complete fee-child custody/global solvency, full ML and production gates remain unqualified. Original failures, held-round and unsafe old literal are not upgraded. Later b657/recovery-v2 source packets and in-progress v10 capture3e0437f1/archive03686b6d are not transferred into these v9b runtime outcomes.

Reviewed input hashes, unchanged through review:

- `SEVENTEENTH-CHECKPOINT.md`: `84f661a1c6369a66018e8596fe5b11d364cbde1fa510fd97da138e8a769ab118`
- `COVERAGE.csv`: `a07b84c98f72644877594d69cb413b0bc644c16c338ac8b71504c87d7bb454fb`
- `batch44-coverage-append.json`: `3676642d88a65c3a20e35be311ad045f4874a9082dd75ce768373469535b7c78`
- `batch44-six-money-v9b-receipt.json`: `528a52d36f55036009b9eb567c3346685b403b97d77ff21fc099921316e9fbdd`
- `batch44-client-postflight.json`: `96e04e64149bbf863c06250df204b4ae454bddde0aba45a183be3f3ead827fc7`
- `batch44-case01-conflict-retained-core-reconciliation.json`: `cedd6a2ba14a9eab6d9df80a6ef3d59acdfd23a67671bba1f70eda1eb391a422`
- `root-j01-replan-retained-core-reconciliation-backend.json`: `bb30ba4304b680622ff06f248ff16f89bdafaacae3c1ecfdb17df955128da07e`
- `batch44-case01-j01-retained-core-final-stability.json`: `8cf261298b7f62b166f6bffae1f1d24408811141eda21cac50e4663ee6e6aac3`
