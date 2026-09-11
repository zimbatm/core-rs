# Verified closure membership API

fstree::verify_membership constructs complete membership during a bounded parallel traversal.
It uses the measured indexed traversal described in closure-membership.md.
The library implementation does not include benchmark counters or verification passes.

VerifiedClosure has private root and MarkSet fields.
Only a successful full traversal constructs it.
The API reads and verifies every reachable interior payload against its content key.
It checks leaf presence in the captured membership snapshot.
It does not hash blob or xattr payloads.

The caller must exclude collection and record replacement during validation.
The same exclusion must cover subsequent certificate construction and use.
A captured mapping or a root pin alone does not establish this exclusion.
Forge's existing server lock supplies it for diagnostic certificate creation.

The result exposes its root and distinct object count.
Conversion to SealedMembership rejects snapshots with active records.
That conversion returns physical membership without the root binding.
The certificate assembler must check the root before consuming the evidence.
The API does not seal, synchronize, sign, publish, or install GC marks.

Each batch contains at most 4,096 interiors.
The jobs argument controls read workers; zero uses available parallelism.
Pending keys and decoded child lists have no independent byte limit.
The API has no previously validated boundaries or partial-success result.

## Validation

All 470 Core tests passed on bld1.
Four new tests cover differential membership, missing records, invalid interiors, and evidence scope.
The wide fixture crosses multiple batches and compares one and eight workers.
Its 16,401 reachable objects include repeated references and exclude an unrelated object.
Serialized membership matches the existing complete traversal exactly.
Tests also cover malformed data, read failure, active-record rejection, and collection after evidence creation.
A deliberate malformed leaf payload confirms the documented presence-only leaf rule.

The gate used forge-dev-91bf04af-d6b3-4c5c-9f0c-b1f6b17f9100.service.
Invocation: 166a1396b1794630b9356a66b21d5d73.
It ended with MainPID 0, Result success, and ExecMainStatus 0.
Only the public concurrency documentation changed after this gate.
The result record preserves tested source hashes and the unchanged code digest.

Forge integration and complete certificate performance measurements remain separate work.
