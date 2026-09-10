# Indexed compaction liveness candidate

Status: tested candidate; not yet pinned by Forge.

Compaction already scans each sealed segment in footer index order.
The previous collector predicate passed each key back to MarkSet::contains.
That repeated segment filtering and key searches during victim selection and copying.

The candidate passes the segment, footer position, and key to the internal liveness predicate.
For a marked slot in the exact captured segment, it reads the bitmap directly.
It checks segment object identity as well as the segment ID.
An ID alone can match a different store or capture.

Unmarked slots still use logical key lookup.
This preserves live older duplicates and keys from segments sealed after capture.
An empty mark set uses a constant false predicate.
The existing grey-set wrapper still protects concurrent writes and published closures.

Both ordinary and verified collector cycles use this internal path.
The public key-predicate compaction interface remains available.
Copy verification, durability ordering, victim selection thresholds, and reference locking retain their existing behavior.

## Verification

All 465 Core tests passed.
The new paired test requires identical compaction statistics and retained payloads through both predicates.
It includes sealed records, an initially active record, a later write, and a greyed existing key.
A second test uses equal segment IDs from different stores.
It requires the fast path to reject the unrelated segment identity.

[Exact validation evidence](indexed-compaction-results.json) includes checked source hashes and the terminal build.
The remote helper receipt names the unchanged Forge checkout.
The evidence records the actual Core source separately.

## Performance and memory limits

Complete compaction speed remains unmeasured.
[Paired record-classification measurements](indexed-liveness.md) improved across all tested live fractions and both layouts.
Live marked records can avoid repeated key searches.
Unmarked records pay the additional slot check before logical lookup.
The comparison covers clustered and mixed marks at five live fractions.
Downstream validation remains necessary before Forge adoption.

Verified collections still retain owned closure keys.
This candidate targets sweep CPU, not that key buffer.
The memory review found that MarkSet owns references to whole mapped segments.
Returning it instead of owned keys would extend those mappings beyond compaction.
It would not produce an independent small bitmap.

Any future key-buffer replacement must preserve stable verified key identity.
Fresh physical membership validation remains necessary after relocation.
The Forge pin stays at 3eac71c5892aeee1443d0a877141f88736924509 pending performance evidence and downstream checks.
