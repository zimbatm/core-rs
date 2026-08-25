//! End-to-end smoke test for the amber-bench example, a port of Go's
//! `cmd/amber-bench/main_test.go`. It builds both example binaries and runs
//! every phase at a size that still seals and reaps packs: 30 refs at a
//! tenth of the sizes over 4 MiB segments (writes ~150 MiB into a tempdir).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Builds the `amber-store` and `amber-bench` example binaries with the
/// profile this test runs under and returns their paths (Go's TestSmoke
/// builds the CLI with the `go` binary; examples have no `CARGO_BIN_EXE_*`,
/// so the test invokes cargo itself and locates `target/<profile>/examples`
/// relative to its own executable).
fn build_examples() -> (PathBuf, PathBuf) {
    let exe = std::env::current_exe().unwrap(); // target/<profile>/deps/<test>-<hash>
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let release = profile_dir.file_name().is_some_and(|n| n == "release");
    let mut args = vec![
        "build",
        "--example",
        "amber-store",
        "--example",
        "amber-bench",
    ];
    if release {
        args.push("--release");
    }
    let st = Command::new(env!("CARGO"))
        .args(&args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("running cargo build");
    assert!(st.success(), "building the example binaries: {st}");
    let examples = profile_dir.join("examples");
    (examples.join("amber-store"), examples.join("amber-bench"))
}

// ---------------------------------------------------------------------------
// TestSmoke (Go: main_test.go).
// ---------------------------------------------------------------------------

#[test]
fn smoke() {
    let (cli, bench) = build_examples();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("results.json");
    let run = Command::new(&bench)
        .arg("--data")
        .arg(dir.path().join("data"))
        .arg("--store")
        .arg(dir.path().join("store"))
        .arg("--bin")
        .arg(&cli)
        .arg("--out")
        .arg(&out)
        .arg("--restore")
        .arg(dir.path().join("restore"))
        .args(["--refs", "30", "--scale", "0.1", "--segment", "4194304"])
        .args(["--phase", "all"])
        .output()
        .expect("running amber-bench");
    assert!(
        run.status.success(),
        "amber-bench --phase all failed ({}):\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let res: serde_json::Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    let ingest = res["Ingest"].as_array().unwrap();
    let delete_n = res["DeleteN"].as_i64().unwrap();
    assert!(
        ingest.len() == 30 && delete_n == 21,
        "ingested {} refs, deleted {delete_n}; want 30 and 21",
        ingest.len()
    );
    let deduped: i64 = ingest.iter().map(|x| x["Deduped"].as_i64().unwrap()).sum();
    assert!(
        deduped > 0,
        "no object deduplicated: the shared clones did not overlap"
    );
    let gc_runs = res["GCRuns"].as_array().unwrap();
    assert_eq!(gc_runs.len(), 2, "{} gc runs, want 2", gc_runs.len());
    let policy_out = gc_runs[0]["Output"].as_str().unwrap();
    assert!(
        policy_out.contains("reaped") && !policy_out.contains(" 0 reaped"),
        "policy gc run reaped nothing: {policy_out:?}"
    );
    let verify_complete = res["VerifyComplete"].as_i64().unwrap();
    let restore_ok = res["VerifyRestoreOK"].as_array().unwrap();
    let verify_errors = res["VerifyErrors"].as_array().cloned().unwrap_or_default();
    assert!(
        verify_complete == 9 && verify_errors.is_empty() && restore_ok.len() == 2,
        "verify: {verify_complete} complete, restores {restore_ok:?}, errors {verify_errors:?}"
    );

    // The report phase re-reads the results file and prints the summary.
    let rep = Command::new(&bench)
        .arg("--out")
        .arg(&out)
        .args(["--phase", "report"])
        .output()
        .expect("running amber-bench report");
    assert!(
        rep.status.success(),
        "report failed: {}",
        String::from_utf8_lossy(&rep.stderr)
    );
    let text = String::from_utf8_lossy(&rep.stdout);
    for want in ["DATASET", "INGEST", "GC RUN", "RECLAIM", "VERIFY"] {
        assert!(text.contains(want), "report lacks {want}:\n{text}");
    }
}
