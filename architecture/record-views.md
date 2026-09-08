# Scoped record views

Records::into_view creates a random-access view from captured record locations.
RecordView::get_record returns only keys selected before capture.
It returns NotFound for other keys, including keys that exist in the source store.
Duplicate selections use one lookup entry.

Construction does not read or copy object payloads.
The view retains handles only for segments containing selected records.
Existing segment handles keep records readable after rotation, compaction, wipe, and store closure.
Dropping the view releases those handles.
A selected record can retain a whole segment after collection unlinks its file.
Consumers must bound view lifetimes and account for those retained bytes.

The view does not add roots to collection or change collection marks.
Tests confirm that collection removes selected keys from the live store while captured reads remain valid.
This uses the same handle lifetime mechanism as Records.

Encoded reads preserve get_record semantics.
They do not authenticate payloads.
Receivers must validate framing, checksums, key identity, and decoded content before publication.
Retaining a handle does not make storage immune to corruption.

The caller supplies the key set.
The type does not prove that the set equals a checkout closure or that a user may access it.
Forge must construct that set under its existing authorization and synchronization rules.
The eventual peer must limit session lifetime and preserve client validation.

## Validation

All core library, integration, and documentation tests passed on 2026-09-08.
Command: TMPDIR=/home/zimbatm/.cache/forge-dev nix develop --command cargo test --quiet.
The library suite contains 427 tests.
Coverage includes excluded existing keys, excluded later keys, empty views, duplicate selections, active and sealed segments, rotation, compaction, wipe, and closure.
The production Forge checkout does not yet use this API.
No end-to-end performance improvement is claimed.
