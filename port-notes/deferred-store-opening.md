# Deferred store opening

Store::begin_open acquires the exclusive directory lock and returns StoreOpening.
The handle has private fields and does not implement Clone.
StoreOpening::finish consumes it and uses the existing validation and recovery path.
Dropping an unfinished handle releases ownership without reading segment contents.
Failed validation releases ownership through normal resource cleanup.

Store::open and Store::open_with_validated_indexes retain their existing behavior.
They acquire the opening handle and immediately finish it.
No format, validation, durability, or collection rule changes.

The split allows a caller to establish exclusive object-store ownership before initializing independent metadata concurrently.
The caller must wait for all initialization results before serving requests.
An error releases the object lock; this API does not coordinate a separate metadata store's failure cleanup.

Three tests cover pending ownership, transfer into the completed store, abandonment, and release after failed validation.
The Core store-check gate passed on bld1.
Unit: forge-dev-879e3fe7-21eb-4302-8a2e-8094403f39e7.service.
Invocation: 3c94ce74e8c646f89bc7d5f9d4368a53.
Terminal state: MainPID 0, Result success, ExecMainStatus 0.
Output: /nix/store/accpvswas8fj5r29xsaf9p35gsfpg4r4-amber-core-store-check-0.1.0.

This branch starts at f18052cd65f973bcee2baf08f3346a471e14f433.
It excludes the separate batch-membership experiment.
No startup speedup is established by the API tests alone.
