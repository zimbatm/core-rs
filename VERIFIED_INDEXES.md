# Verified immutable index startup

Store::open_with_validated_indexes accepts authenticated validation evidence for sealed segments.
Each listed segment must match its fs-verity SHA-256 digest on the exact file descriptor used for mapping.
Core then checks footer layout, bounds, fanout, and filter geometry without repeating the complete footer CRC.
The kernel verifies immutable file pages as they are read.

Unlisted segments retain full footer validation.
A listed segment without fs-verity, or with a different digest, fails store opening.
Ordinary Store::open and Store::open_with retain full validation.
Active recovery, record checks, content addressing, collection, and the storage format remain unchanged.

## Checkpoint contract

The caller owns checkpoint authentication.
A digest measured from a file alone is insufficient evidence.
ValidatedIndexDigest::from_authenticated_bytes requires a verified checkpoint signature, purpose, and segment-ID bindings.
Do not accept raw digests from an untrusted client.

To issue a checkpoint:

1. Capture segment handles with Store::seal_snapshot.
2. Enable fs-verity on those exact handles.
3. Call SegmentSnapshot::validated_index_digests.
4. Authenticate the returned segment-ID and digest map with a versioned index-validation purpose.

The validation method checks every footer after immutability is enabled.
This catches corruption introduced between initial store opening and filesystem sealing.
The map proves index integrity only.
It does not prove payload validity, reachability, completeness, or continued presence after collection.
A stale map cannot make missing segments available.

Core does not enable fs-verity automatically.
Applications must account for checkpoint creation costs and filesystem support.
This API has not yet been connected to Forge checkpoint authentication or measured on nixpkgs.

## Validation

All 436 library tests passed on bld1.
Derivation: /nix/store/5g18qfibil68vdslyc5l5clpzagb5rlz-amber-core-store-check-0.1.0.drv.
The default API retains the existing malformed footer and recovery coverage.
A new test rejects proof restoration for a mutable file.

The Rust index-proof-check example creates a private 256 MiB ext4 image with fs-verity.
It tests valid proof restoration, normal lookup parity, missing keys, and full verification.
It also tests a later unlisted segment, a wrong digest, a mutable copy, and a different immutable replacement.
Corruption introduced after snapshot capture and before filesystem sealing cannot receive validation evidence.

Build derivation: /nix/store/qs6finmrlmrdd19jdaihf7w6d94vw6sq-amber-core-index-proof-check-0.1.0.drv.
Private test unit: forge-index-proof-check-01.service.
Invocation: 390022c2bfbd4366a3565c99952a89c6.
Terminal state: MainPID 0, Result success, ExecMainStatus 0.
All assertions passed, and the image unmounted.
Artifacts remain under /home/zimbatm/forge-dev/index-proof-check-01 on bld1.

Build with nix build .#index-proof-check.
Run index-proof-check with a new absolute output directory inside a private mount namespace.
The probe requires mount privileges and preserves its image after unmounting.
