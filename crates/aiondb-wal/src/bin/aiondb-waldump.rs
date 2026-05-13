use std::io::Read;
use std::path::PathBuf;

use aiondb_wal::{segment, Lsn, WalReader};

#[derive(Debug)]
struct Options {
    wal_dir: PathBuf,
    start_lsn: Lsn,
    limit: Option<usize>,
}

fn usage() -> &'static str {
    "Usage: aiondb-waldump --wal-dir <path> [--start-lsn <lsn>] [--limit <n>]\n\
     \n\
     LSN formats:\n\
       - decimal: 42\n\
       - pg hex: 0/2A\n"
}

fn parse_lsn(value: &str) -> Result<Lsn, String> {
    let token = value.trim();
    Lsn::from_str_value(token).ok_or_else(|| format!("invalid LSN '{token}'"))
}

fn parse_options() -> Result<Options, String> {
    let mut wal_dir: Option<PathBuf> = None;
    let mut start_lsn = Lsn::new(1);
    let mut limit: Option<usize> = None;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--wal-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --wal-dir".to_owned())?;
                wal_dir = Some(PathBuf::from(value));
            }
            "--start-lsn" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --start-lsn".to_owned())?;
                start_lsn = parse_lsn(&value)?;
            }
            "--limit" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --limit".to_owned())?;
                let parsed = value
                    .parse::<usize>()
                    .map_err(|error| format!("invalid --limit value '{value}': {error}"))?;
                limit = Some(parsed);
            }
            other => {
                return Err(format!("unknown argument: {other}\n\n{}", usage()));
            }
        }
    }

    let wal_dir = wal_dir.ok_or_else(|| format!("--wal-dir is required\n\n{}", usage()))?;
    Ok(Options {
        wal_dir,
        start_lsn,
        limit,
    })
}

fn run() -> Result<(), String> {
    let options = parse_options()?;
    print_segment_headers(&options.wal_dir)?;
    let mut reader =
        WalReader::open(options.wal_dir.clone(), options.start_lsn).map_err(|error| {
            format!(
                "failed to open WAL reader for {}: {error}",
                options.wal_dir.display()
            )
        })?;

    let mut emitted = 0usize;
    while let Some((entry, encoded_bytes)) = reader
        .next_entry_with_len()
        .map_err(|error| format!("failed while reading WAL: {error}"))?
    {
        println!(
            "lsn={} prev_lsn={} bytes={} record={:?}",
            entry.lsn.get(),
            entry.prev_lsn.get(),
            encoded_bytes,
            entry.record
        );
        emitted += 1;
        if options.limit.is_some_and(|limit| emitted >= limit) {
            break;
        }
    }

    eprintln!(
        "aiondb-waldump: emitted {} WAL entr{} from {} starting at LSN {}",
        emitted,
        if emitted == 1 { "y" } else { "ies" },
        options.wal_dir.display(),
        options.start_lsn.get()
    );
    Ok(())
}

fn print_segment_headers(wal_dir: &std::path::Path) -> Result<(), String> {
    let segments = segment::list_segments(wal_dir).map_err(|error| {
        format!(
            "failed to list WAL segments in {}: {error}",
            wal_dir.display()
        )
    })?;
    for seg_id in segments {
        let mut file = segment::open_segment_for_read(wal_dir, seg_id).map_err(|error| {
            format!(
                "failed to open WAL segment {} in {}: {error}",
                seg_id.get(),
                wal_dir.display()
            )
        })?;
        let mut probe = vec![0u8; 64];
        let read = file.read(&mut probe).map_err(|error| {
            format!(
                "failed to read WAL segment header {}: {error}",
                seg_id.get()
            )
        })?;
        probe.truncate(read);
        let header = segment::inspect_segment_header(&probe).map_err(|error| {
            format!(
                "failed to parse WAL segment header {}: {error}",
                seg_id.get()
            )
        })?;
        let lsn_mode = match header.lsn_mode {
            Some(aiondb_wal::WalLsnMode::Logical) => "logical",
            Some(aiondb_wal::WalLsnMode::ByteOffset) => "byte_offset",
            None => "unframed/implicit",
        };
        println!(
            "segment={} header_version={} lsn_mode={} system_identifier={} timeline={}",
            seg_id.get(),
            header
                .format_version
                .map_or_else(|| "unframed".to_owned(), |value| value.to_string()),
            lsn_mode,
            header
                .system_identifier
                .map_or_else(|| "-".to_owned(), |value| value.to_string()),
            header
                .timeline_id
                .map_or_else(|| "-".to_owned(), |value| value.to_string()),
        );
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("aiondb-waldump: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_lsn;

    #[test]
    fn parse_lsn_decimal() {
        let lsn = parse_lsn("42").expect("decimal lsn should parse");
        assert_eq!(lsn.get(), 42);
    }

    #[test]
    fn parse_lsn_pg_hex() {
        let lsn = parse_lsn("0/2A").expect("hex lsn should parse");
        assert_eq!(lsn.get(), 42);
    }

    #[test]
    fn parse_lsn_rejects_oversized_high_part() {
        // PG LSN sides are 32-bit; a > 32-bit high part historically
        // overflowed `high << 32` in debug builds. The parser must reject
        // it with a clean error instead.
        let err = parse_lsn("100000000/0").expect_err("oversized high LSN must error");
        assert!(err.contains("invalid LSN"));
    }
}
