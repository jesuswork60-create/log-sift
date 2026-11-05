//! The command line. Everything goes through injected streams and an injected clock, so tests
//! can run it whole without touching the process state.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

use crate::aggregate::{AggSpec, Aggregator};
use crate::formats::Format;
use crate::output::{OutputKind, RowWriter};
use crate::pipeline::{Counters, Flow, OnError, ScanError, Scanner};
use crate::query::Query;
use crate::schema::SchemaCollector;
use crate::time::parse_moment;

pub const USAGE: &str = "usage: log-sift [options] [FILE...]

Reads standard input when no file is given, or for a file named -.

selecting:
  -w, --where QUERY     keep records matching QUERY (repeat to require all)
      --since TIME      from this time, inclusive
      --until TIME      up to this time, exclusive
      --time-field NAME read the time from this field
  -n, --limit N         stop after N matches

reading:
  -f, --format NAME     json, logfmt, nginx or auto (default auto)
      --on-error WHAT   for lines that won't parse: skip, warn, fail, keep (default warn)

results:
  -a, --agg SPEC        aggregate, such as 'count, p95(ms) by status every 5m'
  -c, --count           print how many records matched
      --schema          list the fields found in the input
  -F, --fields A,B      with json, logfmt or table output, only these fields
  -o, --output NAME     raw, json, logfmt or table (default raw, or table for --agg and --schema)
      --width N         in tables, cut cells longer than N characters
      --stats           print line counts to standard error at the end
  -h, --help            this text

TIME is a timestamp, 'now', or a span ago such as 15m or -2h.
QUERY examples: level = error and status >= 500
                path ~ '^/api/' and not exists(user)
                method in (GET, HEAD) or msg contains timeout

Exit status: 0 when something matched, 1 when nothing did, 2 on errors.
";

#[derive(Debug, Default)]
struct Options {
    help: bool,
    wheres: Vec<String>,
    since: Option<String>,
    until: Option<String>,
    time_field: Option<String>,
    limit: Option<u64>,
    format: Option<Format>,
    on_error: Option<OnError>,
    agg: Option<AggSpec>,
    count: bool,
    schema: bool,
    fields: Option<Vec<String>>,
    output: Option<OutputKind>,
    width: Option<usize>,
    stats: bool,
    files: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut rest = args.iter();
    let mut only_files = false;
    while let Some(arg) = rest.next() {
        if only_files || arg == "-" || !arg.starts_with('-') {
            options.files.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_files = true;
            continue;
        }
        let (flag, attached) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            match &attached {
                Some(value) => Ok(value.clone()),
                None => rest.next().cloned().ok_or_else(|| format!("{flag} needs {what}")),
            }
        };
        match flag {
            "-h" | "--help" => options.help = true,
            "-w" | "--where" => options.wheres.push(value("a query")?),
            "--since" => options.since = Some(value("a time")?),
            "--until" => options.until = Some(value("a time")?),
            "--time-field" => options.time_field = Some(value("a field name")?),
            "-n" | "--limit" => {
                let text = value("a number")?;
                options.limit = Some(text.parse().map_err(|_| format!("{flag}: bad number {text:?}"))?);
            }
            "-f" | "--format" => {
                let name = value("a format")?;
                if name != "auto" {
                    options.format = Some(Format::from_name(&name).ok_or_else(|| format!("unknown format {name:?}"))?);
                }
            }
            "--on-error" => {
                let name = value("skip, warn, fail or keep")?;
                options.on_error = Some(OnError::from_name(&name).ok_or_else(|| format!("unknown --on-error value {name:?}"))?);
            }
            "-a" | "--agg" => {
                let text = value("an aggregate")?;
                options.agg = Some(AggSpec::parse(&text).map_err(|message| format!("--agg: {message}"))?);
            }
            "-c" | "--count" => options.count = true,
            "--schema" => options.schema = true,
            "-F" | "--fields" => {
                let names: Vec<String> = value("field names")?.split(',').map(|name| name.trim().to_string()).collect();
                if names.iter().any(String::is_empty) {
                    return Err(format!("{flag}: empty field name"));
                }
                options.fields = Some(names);
            }
            "-o" | "--output" => {
                let name = value("an output format")?;
                options.output = Some(OutputKind::from_name(&name).ok_or_else(|| format!("unknown output {name:?}"))?);
            }
            "--width" => {
                let text = value("a number")?;
                let width: usize = text.parse().map_err(|_| format!("--width: bad number {text:?}"))?;
                if width == 0 {
                    return Err("--width must be at least 1".to_string());
                }
                options.width = Some(width);
            }
            "--stats" => options.stats = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

/// Builds the scanner, or says what is wrong with the options.
fn build_scanner(options: &Options, now: i64) -> Result<Scanner, String> {
    let modes = usize::from(options.agg.is_some()) + usize::from(options.count) + usize::from(options.schema);
    if modes > 1 {
        return Err("--agg, --count and --schema can't be combined".to_string());
    }
    if options.count && (options.output.is_some() || options.fields.is_some()) {
        return Err("--count prints a single number, so it takes no --output or --fields".to_string());
    }
    if options.fields.is_some() && (options.agg.is_some() || options.schema) {
        return Err("--fields applies to matching records, not to --agg or --schema".to_string());
    }
    if options.fields.is_some() && matches!(options.output, None | Some(OutputKind::Raw)) {
        return Err("--fields needs --output json, logfmt or table".to_string());
    }
    if (options.agg.is_some() || options.schema) && options.output == Some(OutputKind::Raw) {
        return Err("--agg and --schema build new rows, so there is no raw line to print".to_string());
    }
    let mut filter: Option<Query> = None;
    for text in &options.wheres {
        let query = Query::parse(text).map_err(|error| format!("bad query {text:?}: {error}"))?;
        filter = Some(match filter {
            Some(existing) => existing.and(query),
            None => query,
        });
    }
    let moment = |text: &Option<String>| text.as_deref().map(|text| parse_moment(text, now)).transpose();
    Ok(Scanner {
        format: options.format,
        filter,
        since: moment(&options.since)?,
        until: moment(&options.until)?,
        time_field: options.time_field.clone(),
        want_time: options.agg.as_ref().is_some_and(|spec| spec.every.is_some()),
        on_error: options.on_error.unwrap_or(OnError::Warn),
        limit: options.limit,
    })
}

/// What a run collects, one of these per mode.
enum Mode<'a> {
    Rows(RowWriter<'a>),
    Count(u64),
    Aggregate(Aggregator),
    Schema(SchemaCollector),
}

/// Runs the program and returns its exit code. `now` is milliseconds since the epoch.
pub fn run(args: &[String], stdin: &mut dyn BufRead, stdout: &mut dyn Write, stderr: &mut dyn Write, now: i64) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => return usage_error(stderr, &message),
    };
    if options.help {
        return match stdout.write_all(USAGE.as_bytes()) {
            Ok(()) => 0,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
            Err(_) => 2,
        };
    }
    let scanner = match build_scanner(&options, now) {
        Ok(scanner) => scanner,
        Err(message) => return usage_error(stderr, &message),
    };
    let default_output = if options.agg.is_some() || options.schema { OutputKind::Table } else { OutputKind::Raw };
    let kind = options.output.unwrap_or(default_output);
    match execute(&options, &scanner, kind, stdin, stdout, stderr) {
        Ok(code) => code,
        // The reader has seen all it wanted, which isn't a failure of ours.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(error) => {
            let _ = writeln!(stderr, "log-sift: writing output: {error}");
            2
        }
    }
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "log-sift: {message}\ntry log-sift --help");
    2
}

fn execute(
    options: &Options,
    scanner: &Scanner,
    kind: OutputKind,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let mut counters = Counters::default();
    let mut failed = false;
    let mut mode = if options.count {
        Mode::Count(0)
    } else if let Some(spec) = &options.agg {
        Mode::Aggregate(Aggregator::new(spec.clone()))
    } else if options.schema {
        Mode::Schema(SchemaCollector::default())
    } else {
        Mode::Rows(RowWriter::new(&mut *stdout, kind, options.fields.clone(), options.width))
    };

    let dash = ["-".to_string()];
    let names: &[String] = if options.files.is_empty() { &dash } else { &options.files };
    for name in names {
        let mut warn = |message: &str| {
            let _ = writeln!(stderr, "log-sift: {message}");
        };
        let mut sink = |entry: &crate::pipeline::Entry| -> io::Result<Flow> {
            match &mut mode {
                Mode::Rows(writer) => writer.write(entry.fields, entry.raw)?,
                Mode::Count(count) => *count += 1,
                Mode::Aggregate(aggregator) => aggregator.add(entry.fields, entry.time),
                Mode::Schema(schema) => schema.add(entry.fields),
            }
            Ok(Flow::Continue)
        };
        let outcome = if name == "-" {
            scanner.scan(&mut *stdin, "(stdin)", &mut counters, &mut warn, &mut sink)
        } else {
            match File::open(name) {
                Ok(file) => scanner.scan(&mut BufReader::new(file), name, &mut counters, &mut warn, &mut sink),
                Err(error) => Err(ScanError::Input { source: name.clone(), error }),
            }
        };
        match outcome {
            Ok(Flow::Continue) => {}
            Ok(Flow::Stop) => break,
            Err(ScanError::Output(error)) => return Err(error),
            Err(error @ ScanError::BadLine { .. }) => {
                let _ = writeln!(stderr, "log-sift: {error}");
                failed = true;
                break;
            }
            // A missing file shouldn't stop the others from being read.
            Err(error) => {
                let _ = writeln!(stderr, "log-sift: {error}");
                failed = true;
            }
        }
    }

    match mode {
        Mode::Rows(writer) => writer.finish()?,
        Mode::Count(count) => writeln!(stdout, "{count}")?,
        Mode::Aggregate(aggregator) => {
            let skipped = aggregator.no_time;
            let rows = aggregator.finish();
            if skipped > 0 {
                let _ = writeln!(stderr, "log-sift: {skipped} records had no time and were left out of the buckets");
            }
            write_rows(stdout, kind, options.width, &rows)?;
        }
        Mode::Schema(schema) => write_rows(stdout, kind, options.width, &schema.finish())?,
    }
    if options.stats {
        let _ = writeln!(stderr, "log-sift: {counters}");
    }
    if failed {
        return Ok(2);
    }
    Ok(if counters.matched > 0 || options.agg.is_some() { 0 } else { 1 })
}

fn write_rows(stdout: &mut dyn Write, kind: OutputKind, width: Option<usize>, rows: &[crate::value::Fields]) -> io::Result<()> {
    let mut writer = RowWriter::new(stdout, kind, None, width);
    for row in rows {
        writer.write(row, "")?;
    }
    writer.finish()
}
