def main [binary: path, store: path, root: string, output: path, --candidate: string = "indexed", --baseline: string = "list", --warmup] {
  let allowed = [list indexed indexed-parallel indexed-batch]
  if $candidate not-in $allowed or $baseline not-in $allowed or $candidate == $baseline {
    error make {msg: "Select two distinct supported modes"}
  }
  if ($output | path exists) { error make {msg: "Use a new result directory"} }
  mkdir $output
  mut samples = []
  mut expected: any = null
  let last = if $warmup { 3 } else { 2 }
  for pair in 0..$last {
    let modes = if $pair mod 2 == 0 { [$baseline $candidate] } else { [$candidate $baseline] }
    for mode in $modes {
      let directory = $output | path join $"($pair)-($mode)"
      {pair: $pair, mode: $mode} | to json | save --force ($output | path join progress.json)
      let result = run-external $binary $store $root $directory $mode | complete
      $result.stderr | save ($output | path join $"($pair)-($mode).log")
      if $result.exit_code != 0 { error make {msg: $result.stderr} }
      let sample = open ($directory | path join result.json)
      let identity = $sample | select root objects interior_reads membership_digest
      if $expected == null {
        $expected = $identity
      } else if $identity != $expected {
        error make {msg: "Closure or membership identity changed"}
      }
      let actual = open --raw ($directory | path join membership.bin)
      let expected_manifest = open --raw ($output | path join $"0-($baseline)" membership.bin)
      if $actual != $expected_manifest { error make {msg: "Membership bytes changed"} }
      $samples = $samples | append ($sample | insert pair $pair | insert warmup ($warmup and $pair == 0))
      {complete: false, samples: $samples} | to json | save --force ($output | path join result.json)
      $sample | select mode total_seconds objects peak_rss_bytes | to json --raw | print
    }
  }
  {complete: true, baseline: $baseline, candidate: $candidate, warmup: $warmup, samples: $samples}
  | to json | save --force ($output | path join result.json)
}
