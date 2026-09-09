# Verified immutable index startup

Store::open_with_validated_indexes accepts authenticated validation evidence for sealed segments.
Each listed segment must match its fs-verity SHA-256 digest on the exact file descriptor used for mapping.
Core defers footer layout, bounds, fanout, and filter geometry checks until each segment is used.
These checks do not repeat the complete footer CRC.
Deferred layout errors propagate from reads, snapshots, verification, and collection.
Mark-set construction and location sorting now return Result to preserve this error propagation.
Marking remains infallible after successful mark-set construction.

Each authenticated segment binds two read-only mappings to the verified inode during opening.
The mappings share file pages but increase virtual address space.
One mapping retains normal scan advice; the other uses random advice for sparse index lookups.
No additional file descriptors remain open.
Only index initialization requires the segment mutex; initialized reads use OnceLock.
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
Forge authenticates these proofs through signed checkpoints.
The lazy initialization change requires new complete-workflow measurements before making performance claims.

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

## Lazy initialization validation

All 443 core library tests passed on bld1 with all features enabled.
The new tests cover concurrent first reads, untouched older indexes, deferred corruption, and retained snapshots.
Ordered record readers retain only required segments.
Existing recovery, collection, verification, and record tests also passed.

Core test derivation: /nix/store/bngsdprp90j5zi51hbwvilgz412832id-amber-core-store-check-0.1.0.drv.
Build unit: forge-dev-4b9b8357-f520-4b61-bc78-927df4e6de6a.service.
Invocation: 3b47c071a2604df992c64939b7b41395.
Terminal state: MainPID 0, Result success, ExecMainStatus 0.

The expanded filesystem probe initializes mark sets, snapshots, verification, and location sorting before any object read.
It also replaces a mapped segment path before its first read.
Reads retain the verified original inode, and snapshot capture rejects the replacement.

Probe derivation: /nix/store/yggx41cz53fcgq81k6xndnb4l6chrln4-amber-core-index-proof-check-0.1.0.drv.
Probe unit: forge-lazy-index-proof-01.service.
Invocation: acf98bef5d54473e937d0423009c5448.
Terminal state: MainPID 0, Result success, ExecMainStatus 0.
All assertions passed, and the private image unmounted.
Artifacts remain under /home/zimbatm/forge-dev/lazy-index-proof-01 on bld1.

Forge integration and complete nixpkgs performance measurements remain pending.
