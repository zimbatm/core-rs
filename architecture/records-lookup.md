# Record lookup order experiment

The records-lookup Rust example compares ordinary key order with sealed-index order.
The candidate sorts by the final key byte, then by the full key.
Both timings include input cloning and physical record ordering.
The candidate timing also includes its extra key sort.

Seven alternating pairs ran on bld1 on 2026-09-08, restricted to CPUs 0–7.
The source was an isolated, stopped nixpkgs object store.
The selected checkout contained 94,342 objects.
The host cache remained warm.

The candidate lost six of seven pairs.
Median ordinary lookup took 71.391266 ms.
Median candidate lookup took 72.976913 ms.
The first ordinary sample took 90.023452 ms.
Its paired candidate took 77.121887 ms.
Later pairs did not reproduce that improvement.

The probe compared all returned encoded records outside the timed section.
Every record and its position matched.
The production lookup order remains unchanged.

This component probe excludes store startup, reachability, record reads, and copying.
It does not establish full checkout latency or cold-storage performance.
The source store contains checkout directory objects created by the previous preparation experiment.

Run:

```text
records-lookup SOURCE_STORE VIEW_STORE ROOT NEW_OUTPUT_DIRECTORY
```

Both stores must be stopped.
Keep all paths under home.
The output directory must be new and absolute.
The probe opens both stores through the normal packstore API and closes them before reporting success.

Runtime: /nix/store/1cfpw5d9b8fc0nxj4yk9g5rg6qbmsvfn-amber-core-records-lookup-0.1.0.
Source parent: 721c7c864374e84353cae3fa583e85aeff41efd3.
Unit: amber-core-records-lookup-01.service.
Invocation: 4be786e997bb44f585bf8b13652d8b48.
The unit finished with SubState=exited, MainPID=0, and ExecMainStatus=0.
Raw results: [records-lookup.json](records-lookup.json).
