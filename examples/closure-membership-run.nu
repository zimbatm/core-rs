def main [binary: path, store: path, root: string, output: path, --candidate: string = "indexed"] {
  if $candidate not-in [indexed indexed-parallel] { error make {msg: "Invalid candidate mode"} }
  if ($output | path exists) { error make {msg: "Use a new result directory"} }
  mkdir $output
  mut samples = []
  mut expected: any = null
  for pair in 0..2 {
    let modes = if $pair mod 2 == 0 { ["list" $candidate] } else { [$candidate "list"] }
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
      let baseline = open --raw ($output | path join 0-list membership.bin)
      if $actual != $baseline { error make {msg: "Membership bytes changed"} }
      $samples = $samples | append ($sample | insert pair $pair)
      {complete: false, samples: $samples} | to json | save --force ($output | path join result.json)
      $sample | select mode total_seconds objects peak_rss_bytes | to json --raw | print
    }
  }
  {complete: true, samples: $samples} | to json | save --force ($output | path join result.json)
}
