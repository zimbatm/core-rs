//! Dev-only mirror of the Go `cmd/amber-store` CLI (no progress UI), for
//! interop testing against the Go binary: same store layout
//! (`<dir>/packstore` + `<dir>/refs`), same spec addressing
//! (`KEY[/PATH]` | `ref:NAME[@PATH]`), same subcommand behavior and
//! `ls -l`-style output.

use std::fs;
use std::io::{self, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};

use amber_store_core::chunkers::ByteOpts;
use amber_store_core::fstree::{self, Entry};
use amber_store_core::ingest;
use amber_store_core::key::Key;
use amber_store_core::packstore;
use amber_store_core::reference::{self, Reference};
use amber_store_core::refstore;
use amber_store_core::{tarexport, tarextract};

type CliError = Box<dyn std::error::Error>;

fn main() {
    if let Err(e) = run() {
        eprintln!("amber-store: {e}");
        std::process::exit(1);
    }
}

/// local content-addressed filesystem tree store
#[derive(Parser)]
#[command(name = "amber-store", disable_help_subcommand = true)]
struct Cli {
    /// store directory (layout: <dir>/packstore, <dir>/refs); defaults to
    /// $AMBER_STORE
    #[arg(long, global = true)]
    store: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// build the content-addressed tree for PATH (a directory or a single
    /// file), store it, and print the root key
    Ingest(IngestArgs),
    /// list the entries of the directory object KEY, or of the subdirectory
    /// PATH within it; accepts a reference as ref:NAME[@PATH]
    Ls(LsArgs),
    /// write the tree rooted at KEY, or at the subdirectory PATH within it,
    /// as a PAX tar to stdout; accepts a reference as ref:NAME[@PATH]
    Export(ExportArgs),
    /// restore the filesystem tree rooted at KEY, or at the subdirectory
    /// PATH within it, into DIR; accepts a reference as ref:NAME[@PATH]
    Restore(RestoreArgs),
    /// manage references: named pointers to root keys
    #[command(subcommand)]
    Ref(RefCmd),
}

#[derive(Args)]
struct IngestArgs {
    /// ultracdc minimum chunk size in bytes
    #[arg(long, default_value_t = 32 << 10)]
    min: i64,
    /// ultracdc average (normal) chunk size in bytes
    #[arg(long, default_value_t = 128 << 10)]
    avg: i64,
    /// ultracdc maximum chunk size in bytes
    #[arg(long, default_value_t = 256 << 10)]
    max: i64,
    /// item chunker average run = 2^bits
    #[arg(long = "item-bits", default_value_t = ingest::DEFAULT_ITEM_BITS)]
    item_bits: u32,
    /// xattrs larger than this many bytes spill to an XattrSet
    #[arg(long = "xattr-inline-max", default_value_t = ingest::DEFAULT_XATTR_INLINE_MAX)]
    xattr_inline_max: usize,
    /// record the resolved root under reference NAME
    #[arg(long = "ref")]
    reference: Option<String>,
    /// concurrent workers building the tree (default: number of CPUs)
    #[arg(long, short = 'j', default_value_t = default_jobs())]
    jobs: usize,
    /// do not honor .amberignore files
    #[arg(long = "no-ignore")]
    no_ignore: bool,
    /// accepted for command-line parity with the Go CLI; this build has no
    /// progress UI
    #[arg(long = "no-progress", hide = true)]
    no_progress: bool,
    path: PathBuf,
}

#[derive(Args)]
struct LsArgs {
    /// append each entry's content key (usable as KEY for ls/export/restore)
    #[arg(long)]
    keys: bool,
    /// KEY[/PATH] | ref:NAME[@PATH]
    spec: String,
}

#[derive(Args)]
struct ExportArgs {
    /// write the tar to FILE instead of stdout
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
    /// KEY[/PATH] | ref:NAME[@PATH]
    spec: String,
}

#[derive(Args)]
struct RestoreArgs {
    /// KEY[/PATH] | ref:NAME[@PATH]
    spec: String,
    /// destination directory
    dir: PathBuf,
}

#[derive(Subcommand)]
enum RefCmd {
    /// list every reference: name, key, creation time, creator
    List,
    /// print the key a reference points at
    Get { name: String },
    /// create or overwrite reference NAME pointing at KEY
    Set { name: String, key: String },
    /// delete reference NAME
    Rm { name: String },
}

fn default_jobs() -> usize {
    thread::available_parallelism().map_or(1, |n| n.get())
}

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Ingest(a) => run_ingest(&cli, a),
        Cmd::Ls(a) => run_ls(&cli, a),
        Cmd::Export(a) => run_export(&cli, a),
        Cmd::Restore(a) => run_restore(&cli, a),
        Cmd::Ref(r) => run_ref(&cli, r),
    }
}

// ---------------------------------------------------------------------------
// Store plumbing (Go: store.go).
// ---------------------------------------------------------------------------

struct Stores {
    objects: packstore::Store,
    refs: refstore::Store,
}

/// Opens (creating as needed) the store directory named by --store or
/// $AMBER_STORE: `<dir>/packstore` holds the objects, `<dir>/refs` the
/// references DB. Stores are single-owner: never open one directory from two
/// live processes.
fn open_store(cli: &Cli) -> Result<Stores, CliError> {
    let dir = match &cli.store {
        Some(d) => d.clone(),
        None => std::env::var("AMBER_STORE").unwrap_or_default(),
    };
    if dir.is_empty() {
        return Err("no store directory: set --store or $AMBER_STORE".into());
    }
    let dir = PathBuf::from(dir);
    let objects =
        packstore::Store::open_with(dir.join("packstore"), packstore::Options::new().sync(true))?;
    let refs = match refstore::Store::open(dir.join("refs"), true) {
        Ok(r) => r,
        Err(e) => {
            let _ = objects.close();
            return Err(e.into());
        }
    };
    Ok(Stores { objects, refs })
}

/// Closes both halves (the refs DB closes on drop).
fn close_store(st: Stores) -> Result<(), CliError> {
    drop(st.refs);
    st.objects.close()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Spec resolution (Go: spec.go).
// ---------------------------------------------------------------------------

/// Parses a content spec: either KEY[/PATH] (lowercase-hex key,
/// slash-separated subpath) or ref:NAME[@PATH] (reference name,
/// '@'-separated subpath — '@' is banned in names, so the first '@' is
/// unambiguous). Reference names resolve through the store's references DB.
fn resolve_spec(refs: &refstore::Store, s: &str) -> Result<(Key, String), CliError> {
    let Some(rest) = s.strip_prefix("ref:") else {
        return parse_key_path(s);
    };
    let (name, path) = match rest.split_once('@') {
        Some((n, p)) => (n, p),
        None => (rest, ""),
    };
    reference::validate_name(name).map_err(|e| format!("invalid reference spec {s:?}: {e}"))?;
    let raw = refs.get(name)?;
    let rec = Reference::decode(&raw).map_err(|e| format!("reference {name:?}: {e}"))?;
    let k = Key::parse(&rec.key).map_err(|e| format!("reference {name:?}: stored key: {e}"))?;
    Ok((k, path.to_string()))
}

/// Splits a KEY[/PATH] argument at the first slash and decodes the key part.
/// The returned path is empty when no slash follows the key.
fn parse_key_path(s: &str) -> Result<(Key, String), CliError> {
    let (key_part, path) = match s.split_once('/') {
        Some((k, p)) => (k, p),
        None => (s, ""),
    };
    Ok((parse_hex_key(key_part)?, path.to_string()))
}

/// Decodes a lowercase-hex key argument into a validated key.
fn parse_hex_key(s: &str) -> Result<Key, CliError> {
    let raw = hex::decode(s).map_err(|e| format!("invalid key {s:?}: {e}"))?;
    let k = Key::parse(&raw).map_err(|e| format!("invalid key {s:?}: {e}"))?;
    Ok(k)
}

/// Resolves a slash-separated subpath from `root` and returns the target
/// entry's content key. Every traversed segment must be an entry carrying a
/// content key (a regular file or a directory).
fn descend(objects: &packstore::Store, root: Key, path: &str) -> Result<Key, CliError> {
    let mut k = root;
    for seg in path.split('/') {
        if seg.is_empty() {
            continue;
        }
        let e = fstree::lookup_entry(k, seg.as_bytes(), |kk| objects.get(kk))
            .map_err(|e| format!("resolving {path:?}: {e}"))?;
        k = Key::parse(&e.content_key)
            .map_err(|_| format!("resolving {path:?}: {seg:?} is not a file or directory"))?;
    }
    Ok(k)
}

// ---------------------------------------------------------------------------
// ingest (Go: ingest.go + the chunk flags in main.go).
// ---------------------------------------------------------------------------

/// Maps the CLI chunking flags onto the library options. min/avg/max must
/// all be set together or all left zero (the library defaults).
fn chunk_opts(a: &IngestArgs) -> Result<ingest::ChunkOpts, CliError> {
    let mut opts = ingest::ChunkOpts {
        byte: None,
        item_bits: a.item_bits,
        xattr_inline_max: a.xattr_inline_max,
    };
    if a.min == 0 && a.avg == 0 && a.max == 0 {
        return Ok(opts);
    }
    if a.min <= 0 || a.avg <= 0 || a.max <= 0 {
        return Err("--min, --avg and --max must all be set together".into());
    }
    opts.byte = Some(ByteOpts {
        min_size: a.min as usize,
        normal_size: a.avg as usize,
        max_size: a.max as usize,
        key: Vec::new(),
    });
    Ok(opts)
}

fn run_ingest(cli: &Cli, a: &IngestArgs) -> Result<(), CliError> {
    if let Some(name) = &a.reference {
        reference::validate_name(name)?;
    }
    let chunk = chunk_opts(a)?;
    let st = open_store(cli)?;
    let opts = ingest::Opts {
        jobs: a.jobs,
        chunk,
        no_ignore: a.no_ignore,
        progress: None,
    };
    let (_stats, res) = ingest::dir(&st.objects, &a.path, opts);
    let root = match res {
        Ok(k) => k,
        Err(e) => {
            let _ = close_store(st);
            return Err(e.into());
        }
    };
    if let Some(name) = &a.reference {
        let rec = Reference {
            name: name.clone(),
            key: root.as_bytes().to_vec(),
            created_at: now_unix_nanos(),
            ..Default::default()
        };
        let put = rec.encode().map_err(CliError::from).and_then(|raw| {
            st.refs.put(name, &raw)?;
            Ok(())
        });
        if let Err(e) = put {
            let _ = close_store(st);
            return Err(format!(
                "tree stored (root {root}) but creating reference {name:?} failed: {e}\n\
                 retry with: amber-store ref set {name:?} {root}"
            )
            .into());
        }
    }
    close_store(st)?;
    println!("{root}");
    Ok(())
}

fn now_unix_nanos() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => -(e.duration().as_nanos() as i64),
    }
}

// ---------------------------------------------------------------------------
// ls (Go: ls.go).
// ---------------------------------------------------------------------------

/// Bounds one list_entries page; run_ls loops until the listing is drained,
/// so it only caps memory per fetch, not the output.
const LS_PAGE_SIZE: usize = 4096;

fn run_ls(cli: &Cli, a: &LsArgs) -> Result<(), CliError> {
    let st = open_store(cli)?;
    let res = ls_inner(&st, a);
    let close = close_store(st);
    res?;
    close
}

fn ls_inner(st: &Stores, a: &LsArgs) -> Result<(), CliError> {
    let (k, path) = resolve_spec(&st.refs, &a.spec)?;
    let dir = descend(&st.objects, k, &path)?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut after: Vec<u8> = Vec::new();
    loop {
        let (page, more) =
            fstree::list_entries(dir, &after, LS_PAGE_SIZE, |kk| st.objects.get(kk))?;
        entries.extend(page);
        if !more {
            break;
        }
        after = entries.last().map(|e| e.name.clone()).unwrap_or_default();
    }
    render_ls(&mut io::stdout().lock(), &entries, now_unix_nanos(), a.keys)?;
    Ok(())
}

/// Writes one `ls -l` style line per entry: mode, uid, gid, size, mtime and
/// name (with the symlink target after "->"). Numeric columns are
/// right-aligned to the widest value. When `show_keys` is set, each entry's
/// content key (if any) is appended after the name. Names are raw bytes,
/// exactly as Go prints them.
fn render_ls(
    w: &mut dyn io::Write,
    entries: &[Entry],
    now_ns: i64,
    show_keys: bool,
) -> io::Result<()> {
    let (mut uid_w, mut gid_w, mut size_w) = (0, 0, 0);
    for e in entries {
        uid_w = uid_w.max(e.uid.to_string().len());
        gid_w = gid_w.max(e.gid.to_string().len());
        size_w = size_w.max(size_string(e).len());
    }
    for e in entries {
        let mut line = Vec::new();
        line.extend_from_slice(mode_string(e.mode).as_bytes());
        line.extend_from_slice(
            format!(
                " {:>uw$} {:>gw$} {:>sw$} {} ",
                e.uid,
                e.gid,
                size_string(e),
                format_mtime(e.mtime, now_ns),
                uw = uid_w,
                gw = gid_w,
                sw = size_w,
            )
            .as_bytes(),
        );
        line.extend_from_slice(&e.name);
        if !e.link_target.is_empty() {
            line.extend_from_slice(b" -> ");
            line.extend_from_slice(&e.link_target);
        }
        // An absent (empty) content key simply fails to parse.
        let shown_key = if show_keys {
            Key::parse(&e.content_key).ok()
        } else {
            None
        };
        if let Some(ck) = shown_key {
            line.push(b' ');
            line.extend_from_slice(ck.to_string().as_bytes());
        }
        line.push(b'\n');
        w.write_all(&line)?;
    }
    Ok(())
}

/// Renders the size column: "major,minor" for device entries, the content
/// length carried by the entry's key otherwise (a directory key's length
/// counts its entries). Symlinks show their target length.
fn size_string(e: &Entry) -> String {
    if e.rdev.len() == 2 {
        return format!("{},{}", e.rdev[0], e.rdev[1]);
    }
    // An absent (empty) content key simply fails to parse, so no emptiness
    // pre-check is needed.
    if let Ok(ck) = Key::parse(&e.content_key) {
        return ck.length().to_string();
    }
    e.link_target.len().to_string()
}

const S_IFMT: u64 = libc::S_IFMT as u64;
const S_IFDIR: u64 = libc::S_IFDIR as u64;
const S_IFLNK: u64 = libc::S_IFLNK as u64;
const S_IFCHR: u64 = libc::S_IFCHR as u64;
const S_IFBLK: u64 = libc::S_IFBLK as u64;
const S_IFIFO: u64 = libc::S_IFIFO as u64;
const S_IFSOCK: u64 = libc::S_IFSOCK as u64;
const S_IFREG: u64 = libc::S_IFREG as u64;
const S_ISUID: u64 = libc::S_ISUID as u64;
const S_ISGID: u64 = libc::S_ISGID as u64;
const S_ISVTX: u64 = libc::S_ISVTX as u64;

/// Renders a raw POSIX st_mode the way `ls -l` does: a type character
/// followed by nine permission characters, with setuid/setgid/sticky folded
/// into the corresponding execute slots.
fn mode_string(mode: u64) -> String {
    let mut b = [0u8; 10];
    b[0] = match mode & S_IFMT {
        S_IFDIR => b'd',
        S_IFLNK => b'l',
        S_IFCHR => b'c',
        S_IFBLK => b'b',
        S_IFIFO => b'p',
        S_IFSOCK => b's',
        S_IFREG => b'-',
        _ => b'?',
    };
    const RWX: &[u8; 9] = b"rwxrwxrwx";
    for (i, &c) in RWX.iter().enumerate() {
        b[1 + i] = if mode & (1 << (8 - i)) != 0 { c } else { b'-' };
    }
    let mut set_bit = |pos: usize, bit: u64, with_x: u8, without_x: u8| {
        if mode & bit == 0 {
            return;
        }
        b[pos] = if b[pos] == b'x' { with_x } else { without_x };
    };
    set_bit(3, S_ISUID, b's', b'S');
    set_bit(6, S_ISGID, b's', b'S');
    set_bit(9, S_ISVTX, b't', b'T');
    String::from_utf8_lossy(&b).into_owned()
}

/// The local calendar fields of a Unix timestamp, via libc so the timezone
/// database is consulted exactly like Go's time package does.
fn local_tm(secs: i64) -> libc::tm {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r fills the out-param and touches nothing else.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Renders an mtime the way `ls -l` (and the Go CLI) does: "Jan _2 15:04"
/// for times within the last six months, "Jan _2  2006" for older or future
/// times. The six-month cutoff mirrors Go's `now.AddDate(0, -6, 0)` via
/// mktime's field normalization.
fn format_mtime(t_ns: i64, now_ns: i64) -> String {
    let cutoff_ns = {
        let mut tm = local_tm(now_ns.div_euclid(1_000_000_000));
        tm.tm_mon -= 6;
        tm.tm_isdst = -1;
        // SAFETY: mktime normalizes the tm we own; no aliasing.
        let secs = unsafe { libc::mktime(&mut tm) } as i64;
        secs * 1_000_000_000 + now_ns.rem_euclid(1_000_000_000)
    };
    let tm = local_tm(t_ns.div_euclid(1_000_000_000));
    let mon = MONTHS
        .get(tm.tm_mon.clamp(0, 11) as usize)
        .copied()
        .unwrap_or("???");
    if t_ns > now_ns || t_ns < cutoff_ns {
        format!("{mon} {:>2}  {}", tm.tm_mday, i64::from(tm.tm_year) + 1900)
    } else {
        format!("{mon} {:>2} {:02}:{:02}", tm.tm_mday, tm.tm_hour, tm.tm_min)
    }
}

// ---------------------------------------------------------------------------
// export / restore (Go: export.go, restore.go).
// ---------------------------------------------------------------------------

fn run_export(cli: &Cli, a: &ExportArgs) -> Result<(), CliError> {
    let st = open_store(cli)?;
    let res = export_inner(&st, a);
    let close = close_store(st);
    res?;
    close
}

fn export_inner(st: &Stores, a: &ExportArgs) -> Result<(), CliError> {
    let (k, path) = resolve_spec(&st.refs, &a.spec)?;
    let dir = descend(&st.objects, k, &path)?;
    match &a.output {
        Some(out) => {
            let mut f = fs::File::create(out)?;
            tarexport::write(&mut f, dir, |kk| st.objects.get(kk))?;
            f.sync_all()?; // surface a flush error on the happy path
            Ok(())
        }
        None => {
            let mut w = io::stdout().lock();
            tarexport::write(&mut w, dir, |kk| st.objects.get(kk))?;
            Ok(())
        }
    }
}

fn run_restore(cli: &Cli, a: &RestoreArgs) -> Result<(), CliError> {
    let st = open_store(cli)?;
    let res = restore_inner(&st, a);
    let close = close_store(st);
    res?;
    close
}

fn restore_inner(st: &Stores, a: &RestoreArgs) -> Result<(), CliError> {
    let (k, path) = resolve_spec(&st.refs, &a.spec)?;
    let dir = descend(&st.objects, k, &path)?;

    // The Go CLI pipes tarexport straight into tarextract. Here the export
    // is spooled through an unlinked temp file instead of a pipe (the
    // toolchain floor predates std::io::pipe), which keeps memory flat and
    // surfaces an export error before extraction starts, like the pipe's
    // CloseWithError does.
    let mut spool = tempfile::tempfile()?;
    tarexport::write(&mut spool, dir, |kk| st.objects.get(kk))?;
    spool.seek(SeekFrom::Start(0))?;
    tarextract::extract(&mut spool, &a.dir)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// ref (Go: ref.go).
// ---------------------------------------------------------------------------

fn run_ref(cli: &Cli, cmd: &RefCmd) -> Result<(), CliError> {
    match cmd {
        RefCmd::List => {
            let st = open_store(cli)?;
            let res = ref_list(&st);
            let close = close_store(st);
            res?;
            close
        }
        RefCmd::Get { name } => {
            let st = open_store(cli)?;
            let res = resolve_spec(&st.refs, &format!("ref:{name}"));
            let close = close_store(st);
            let (k, _) = res?;
            println!("{k}");
            close
        }
        RefCmd::Set { name, key } => {
            let k = parse_hex_key(key)?;
            let rec = Reference {
                name: name.clone(),
                key: k.as_bytes().to_vec(),
                created_at: now_unix_nanos(),
                ..Default::default()
            };
            let raw = rec.encode()?;
            let st = open_store(cli)?;
            let res = st.refs.put(name, &raw);
            let close = close_store(st);
            res?;
            close
        }
        RefCmd::Rm { name } => {
            let st = open_store(cli)?;
            let res = st.refs.delete(name);
            let close = close_store(st);
            res?;
            close
        }
    }
}

fn ref_list(st: &Stores) -> Result<(), CliError> {
    let records = st.refs.all()?;
    let mut out = io::stdout().lock();
    for r in records {
        let rec = Reference::decode(&r.data).map_err(|e| format!("reference {:?}: {e}", r.name))?;
        let k =
            Key::parse(&rec.key).map_err(|e| format!("reference {:?}: stored key: {e}", r.name))?;
        let mut line = format!("{} {} {}", rec.name, k, rfc3339_utc(rec.created_at));
        if !rec.user.is_empty() {
            line.push(' ');
            line.push_str(&rec.user);
        }
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Formats a ns-precision Unix timestamp the way Go's
/// `time.Unix(0, ns).UTC().Format(time.RFC3339)` does (seconds precision,
/// trailing "Z").
fn rfc3339_utc(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Proleptic-Gregorian date from days since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}
