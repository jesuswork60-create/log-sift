# log-sift

Answering questions about a log file usually starts with a pile of `grep` and `awk`, and that falls apart once the values you care about sit inside JSON or need comparing as numbers. log-sift reads each line as a record, so you can ask `status >= 500` instead of writing a regex for it.

It reads JSON lines, logfmt and the nginx/Apache "combined" access log format, working out which one by looking at the first lines of each input. On top of that are a small query language, time windows, and aggregates with grouping and time buckets. It's a single binary with no dependencies outside the Rust standard library.

## Install

You need a Rust toolchain (stable).

```
cargo build --release
```

The binary ends up at `target/release/log-sift`. `cargo install --path .` puts it on your path instead. The commands below use three small made-up logs from `examples/`.

## Usage

Filter a file. Matching lines come out exactly as they went in, so the result still works with whatever you pipe it to:

```
$ log-sift -w 'level = error' examples/app.jsonl
{"timestamp":"2024-03-04T09:02:40Z","level":"error","service":"payments","msg":"card gateway timeout","retry":true,"ms":3001}
{"timestamp":"2024-03-04T09:05:29Z","level":"error","service":"payments","msg":"card gateway timeout","retry":false,"ms":3004}
```

Pick out fields as a table, here from an access log:

```
$ log-sift -w 'status >= 400' -o table -F remote_addr,status,path examples/access.log
remote_addr  status  path
192.0.2.55      404  /wp-login.php
192.0.2.55      404  /.env
203.0.113.7     502  /api/search?q=kettle
```

Summarize:

```
$ log-sift -a 'count, avg(ms), p95(ms), max(ms) by service' examples/app.jsonl
service   count            avg(ms)  p95(ms)  max(ms)
checkout      3                 38     40.7       41
payments      2             3002.5  3003.85     3004
search        3  311.3333333333333    742.8      812

$ log-sift -a 'count, distinct(service) every 5m' examples/app.jsonl
bucket                count  distinct(service)
2024-03-04T09:00:00Z      5                  3
2024-03-04T09:05:00Z      3                  3
```

And if you don't know what's in a file yet:

```
$ log-sift --schema examples/service.logfmt | head -n 4
field      count  percent  types  example
component      4      100  text   worker
level          4      100  text   info
msg            4      100  text   job started
```

`log-sift --help` lists every option.

### Queries

| Form | Example |
| --- | --- |
| comparison: `= == != < <= > >=` | `status >= 500` |
| regular expression: `~ !~` | `path ~ '^/api/'` |
| text: `contains startswith endswith glob` | `msg contains timeout`, `host glob 'web-*'` |
| list: `in`, `not in` | `method in (GET, HEAD)` |
| presence: `exists(...)` | `not exists(user)` |
| combine: `and or not`, or `&& \|\| !`, and brackets | `(a = 1 or b = 2) and not c = 3` |

Nested values use dots, and a number indexes into an array: `order.total`, `tags.0`. An exact key containing a dot wins over the nested reading. Names that aren't plain words go in backticks, such as `` `content-type` ``.

A few rules worth knowing:

- A field that's missing fails every test, `!=` and `!~` included. Use `not exists(x)` to find those records.
- Text that looks like a number compares as a number, so `status >= 500` works on logfmt too, where every value is text.
- An array matches when any of its items does. The negative tests (`!~`, `not in`) mean that no item does.
- Unquoted words on the right are text, so `level = error` needs no quotes. The right side can't name another field.

Regular expressions come from a small engine in this crate. It supports classes, `\d \w \s \b`, anchors, `* + ? {n,m}`, alternation and groups, plus a leading `(?i)`. It runs in linear time, and in exchange has no backreferences or lookaround.

### Aggregates

`--agg` takes `METRICS [by FIELD, ...] [every SPAN]`. The metrics are `count`, `count(f)`, `sum(f)`, `avg(f)`, `min(f)`, `max(f)`, `distinct(f)` and percentiles such as `p50(f)` or `p99.9(f)`.

### Time

`--since` and `--until` take a timestamp, `now`, or a span back from now such as `15m` or `-2h`. The window includes `--since` and excludes `--until`. The time of a record comes from `--time-field`, or else from the first of `timestamp`, `@timestamp`, `time`, `ts`, `date` and `t` that parses. Numbers in those fields are read as epoch seconds or milliseconds depending on their size. Records with no readable time are left out when a window is set, and `--stats` shows how many.

### Exit status

0 when something matched, or always for `--agg`. 1 when nothing matched. 2 for bad options, a file that couldn't be read, or a bad line under `--on-error fail`. A file that can't be opened doesn't stop the other files from being read.

## As a library

The crate is also a library called `logsift`. The pieces can be used on their own:

```rust
use logsift::query::Query;
use logsift::formats::Format;

let fields = Format::Logfmt.parse_line("level=error status=503 path=/api/items").unwrap();
let query = Query::parse("level = error and status >= 500").unwrap();
assert!(query.matches(&fields));
```

## Tests

```
cargo test
```

Unit tests sit next to the code, `tests/cli.rs` runs the whole command line in-process against a fixed clock, and `tests/binary.rs` starts the real program to check exit codes and pipes.

## Known limitations

- Every number is an `f64`, so integers beyond 2^53 lose precision, and sums are plain floating-point additions.
- Timestamps with no offset are taken as UTC, and there's no setting to change that.
- Percentiles and `distinct` are exact, which means they hold their values in memory. So do grouped aggregates and table output, which keep everything until the end of the input.
- A file has one format, chosen from its first 16 non-blank lines. A mixed file reports the other lines as errors. Detection can also be fooled: prose containing a `key=value` counts as logfmt.
- Files are read one after another. Nothing merges them by time, and there's no way to follow a growing file.
- Time buckets with no records are absent from the output rather than shown as zero.
- Groups compare by displayed text, so the number `200` and the text `"200.0"` are separate groups.
- nginx lines need a number in the size field. Apache's `-` for "no body" is rejected as a bad line.
- `\w` and `\b` only know ASCII, and `(?i)` only works at the very start of a pattern.
- Table columns are measured in characters, so wide scripts and emoji won't line up.
- There's no syslog format and no compressed input.
