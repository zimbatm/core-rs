# Verified collection closure capture

Collector::run_verified adds an optional closure-evidence path to collection.
It captures roots and acquires independent root pins under the collector reference lock.
The mark checks interior hashes before decoding children and records each newly marked key once.
Leaf keys retain presence semantics.
Ordinary run and advisory status keep their existing marking behavior.

Only keys reached from the initial root snapshot enter the returned closure.
Concurrent reference and raw-object write-barrier additions remain excluded from this evidence.
The ordinary barrier still preserves those additions during sweeping.

A private VerifiedCollection value is returned only after successful compaction.
It exposes cycle statistics, captured roots, and closure keys.
The handle borrows its collector and retains the captured roots until drop.
Independent handles and existing reference pins keep independent counts.
Errors drop temporary pins and return no capability.

The capability supplies logical closure evidence, not authenticated repository identity or physical membership.
A consumer must authenticate its repository claim and require its root among the captured roots.
It must capture and validate current destination physical membership before persisting a checkpoint.
Concurrent collection must remain excluded during that physical evidence installation.

The optional key vector increases temporary memory.
This prototype has no large-repository timing or memory acceptance evidence.
It does not change persistent formats, collection thresholds, or durable retention policy.

## Validation

All 463 Core library tests passed on bld1, with no failures or ignored tests.
The four new tests cover empty roots, corrupt-interior rejection and failure cleanup, exclusion of an unclosed unpublished barrier object, and independent retained-root release.
The checked source hashes match the published Rust files.
Formatting passed.

Gate: forge-dev-c594a48e-3813-488a-abf9-06a926698a69.service.
Invocation: c9c602ded42644c6825e16ec5f241b11.
Terminal state: MainPID 0, Result success, ExecMainStatus 0.
The remote orchestration receipt names the unchanged Forge checkout; the evidence file records the actual Core source and hashes.
Base Core revision: f18052cd65f973bcee2baf08f3346a471e14f433.
This remains a Core experiment; Forge has not adopted this revision.
See verified-collection-results.json for the checked source hashes and test results.
