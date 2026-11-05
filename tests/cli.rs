//! Runs the whole command line in process, against in-memory streams and a fixed clock.

use std::fs;
use std::path::PathBuf;

use logsift::cli::run;
use logsift::time::parse_text;

struct Outcome {
    code: i32,
    out: String,
    err: String,
}

fn sift(args: &[&str], input: &str) -> Outcome {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdin = input.as_bytes();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let now = parse_text("2024-03-04T09:10:00Z").unwrap();
    let code = run(&args, &mut stdin, &mut out, &mut err, now);
    Outcome {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn example(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "examples", name].iter().collect();
    path.to_str().unwrap().to_string()
}

fn sift_example(name: &str, args: &[&str]) -> Outcome {
    let file = example(name);
    let mut all: Vec<&str> = args.to_vec();
    all.push(&file);
    sift(&all, "")
}

const SMALL: &str = "{\"level\":\"info\",\"n\":1}\n{\"level\":\"error\",\"n\":2}\n{\"level\":\"error\",\"n\":3}\n";

#[test]
fn with_no_options_every_record_comes_back_as_written() {
    let outcome = sift(&[], SMALL);
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.out, SMALL);
    assert_eq!(outcome.err, "");
}

#[test]
fn where_filters() {
    let outcome = sift(&["-w", "level = error and n > 2"], SMALL);
    assert_eq!(outcome.out, "{\"level\":\"error\",\"n\":3}\n");
}

#[test]
fn repeated_where_requires_all_of_them() {
    let outcome = sift(&["--where", "level = error", "--where=n < 3"], SMALL);
    assert_eq!(outcome.out, "{\"level\":\"error\",\"n\":2}\n");
}

#[test]
fn no_match_exits_one_with_empty_output() {
    let outcome = sift(&["-w", "level = fatal"], SMALL);
    assert_eq!((outcome.code, outcome.out.as_str()), (1, ""));
}

#[test]
fn a_bad_query_is_a_usage_error_with_its_position() {
    let outcome = sift(&["-w", "level = "], SMALL);
    assert_eq!(outcome.code, 2);
    assert!(outcome.err.contains("bad query"), "{}", outcome.err);
    assert!(outcome.err.contains("character 8"), "{}", outcome.err);
}

#[test]
fn unknown_options_and_missing_values_are_usage_errors() {
    assert_eq!(sift(&["--nope"], "").code, 2);
    assert_eq!(sift(&["-w"], "").code, 2);
    assert_eq!(sift(&["-n", "many"], "").code, 2);
    assert_eq!(sift(&["-f", "yaml"], "").code, 2);
    assert_eq!(sift(&["-o", "xml"], "").code, 2);
    assert_eq!(sift(&["--on-error", "panic"], "").code, 2);
    assert_eq!(sift(&["--width", "0"], "").code, 2);
    assert_eq!(sift(&["--since", "someday"], "").code, 2);
    assert_eq!(sift(&["-a", "median(ms)"], "").code, 2);
}

#[test]
fn conflicting_modes_are_refused() {
    assert_eq!(sift(&["-c", "--schema"], "").code, 2);
    assert_eq!(sift(&["-c", "-a", "count"], "").code, 2);
    assert_eq!(sift(&["-c", "-o", "json"], "").code, 2);
    assert_eq!(sift(&["-a", "count", "-F", "x", "-o", "json"], "").code, 2);
    assert_eq!(sift(&["--schema", "-o", "raw"], "").code, 2);
    assert_eq!(sift(&["-F", "x"], "").code, 2);
}

#[test]
fn help_goes_to_standard_output() {
    let outcome = sift(&["--help"], "");
    assert_eq!(outcome.code, 0);
    assert!(outcome.out.starts_with("usage: log-sift"));
}

#[test]
fn count_prints_a_number() {
    let outcome = sift(&["-c", "-w", "level = error"], SMALL);
    assert_eq!((outcome.code, outcome.out.as_str()), (0, "2\n"));
    let none = sift(&["-c", "-w", "level = fatal"], SMALL);
    assert_eq!((none.code, none.out.as_str()), (1, "0\n"));
}

#[test]
fn limit_stops_at_the_given_number() {
    assert_eq!(sift(&["-n", "1", "-c"], SMALL).out, "1\n");
    assert_eq!(sift(&["-n", "2"], SMALL).out.lines().count(), 2);
}

#[test]
fn limit_of_zero_reads_nothing() {
    let outcome = sift(&["-n", "0"], SMALL);
    assert_eq!((outcome.code, outcome.out.as_str()), (1, ""));
}

#[test]
fn json_and_logfmt_output_with_fields() {
    let json = sift(&["-o", "json", "-F", "n", "-n", "1"], SMALL);
    assert_eq!(json.out, "{\"n\":1}\n");
    let logfmt = sift(&["-o", "logfmt", "--fields=level,n", "-n", "1"], SMALL);
    assert_eq!(logfmt.out, "level=info n=1\n");
}

#[test]
fn logfmt_input_converts_to_json() {
    let outcome = sift(&["-f", "logfmt", "-o", "json"], "a=1 b=\"x y\"\n");
    assert_eq!(outcome.out, "{\"a\":\"1\",\"b\":\"x y\"}\n");
}

#[test]
fn table_output_lines_up() {
    let outcome = sift(&["-o", "table", "-F", "level,n"], SMALL);
    assert_eq!(outcome.out, "level  n\ninfo   1\nerror  2\nerror  3\n");
}

#[test]
fn width_cuts_table_cells() {
    let outcome = sift(&["-o", "table", "-F", "level", "--width", "3", "-n", "1", "-w", "level = error"], SMALL);
    assert_eq!(outcome.out, "level\ner…\n");
}

#[test]
fn aggregates_default_to_a_table() {
    let outcome = sift(&["-a", "count, sum(n) by level"], SMALL);
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.out, "level  count  sum(n)\nerror      2       5\ninfo       1       1\n");
}

#[test]
fn aggregates_can_be_json() {
    let outcome = sift(&["-a", "max(n)", "-o", "json"], SMALL);
    assert_eq!(outcome.out, "{\"max(n)\":3}\n");
}

#[test]
fn a_plain_aggregate_over_nothing_still_answers_and_exits_zero() {
    let outcome = sift(&["-a", "count", "-w", "level = fatal", "-o", "json"], SMALL);
    assert_eq!((outcome.code, outcome.out.as_str()), (0, "{\"count\":0}\n"));
}

#[test]
fn time_buckets_come_from_the_records() {
    let outcome = sift_example("app.jsonl", &["-a", "count every 5m", "-o", "logfmt"]);
    assert_eq!(outcome.out, "bucket=2024-03-04T09:00:00Z count=5\nbucket=2024-03-04T09:05:00Z count=3\n");
}

#[test]
fn records_without_a_time_are_reported_when_bucketing() {
    let outcome = sift(&["-a", "count every 1m", "-f", "logfmt", "-o", "logfmt"], "ts=2024-01-01T00:00:30Z\nn=1\n");
    assert_eq!(outcome.out, "bucket=2024-01-01T00:00:00Z count=1\n");
    assert!(outcome.err.contains("1 records had no time"), "{}", outcome.err);
}

#[test]
fn since_and_until_understand_spans_and_now() {
    let outcome = sift_example("app.jsonl", &["-c", "--since", "5m", "--until", "now"]);
    assert_eq!(outcome.out, "3\n");
    let older = sift_example("app.jsonl", &["-c", "--until", "-5m"]);
    assert_eq!(older.out, "5\n");
}

#[test]
fn since_takes_a_timestamp() {
    let outcome = sift_example("app.jsonl", &["-c", "--since=2024-03-04T09:07:00Z"]);
    assert_eq!(outcome.out, "1\n");
}

#[test]
fn time_field_picks_the_field() {
    let input = "{\"when\":\"2024-01-02\",\"n\":1}\n{\"time\":\"2024-01-02\",\"n\":2}\n";
    let outcome = sift(&["--since", "2024-01-01", "--time-field", "when", "-o", "json", "-F", "n"], input);
    assert_eq!(outcome.out, "{\"n\":1}\n");
}

#[test]
fn bad_lines_warn_by_default_and_the_run_still_succeeds() {
    let outcome = sift(&["-f", "json"], "{\"a\":1}\nbroken\n");
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.out, "{\"a\":1}\n");
    assert!(outcome.err.contains("(stdin):2:"), "{}", outcome.err);
}

#[test]
fn on_error_fail_stops_with_exit_two() {
    let outcome = sift(&["-f", "json", "--on-error", "fail"], "{\"a\":1}\nbroken\n{\"a\":2}\n");
    assert_eq!(outcome.code, 2);
    assert_eq!(outcome.out, "{\"a\":1}\n");
}

#[test]
fn on_error_skip_is_quiet() {
    let outcome = sift(&["-f", "json", "--on-error=skip", "--stats"], "broken\n");
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.err, "log-sift: lines=1 blank=0 bad=1 no_time=0 matched=0\n");
}

#[test]
fn on_error_keep_makes_bad_lines_queryable() {
    let outcome = sift(&["-f", "json", "--on-error", "keep", "-w", "exists(_error)", "-o", "json", "-F", "_raw"], "{\"a\":1}\nbroken\n");
    assert_eq!(outcome.out, "{\"_raw\":\"broken\"}\n");
}

#[test]
fn a_missing_file_is_reported_and_others_are_still_read() {
    let good = example("service.logfmt");
    let outcome = sift(&["-c", "no-such-file.log", &good], "");
    assert_eq!(outcome.code, 2);
    assert_eq!(outcome.out, "4\n");
    assert!(outcome.err.contains("no-such-file.log"), "{}", outcome.err);
}

#[test]
fn dash_reads_standard_input_between_files() {
    let good = example("service.logfmt");
    let outcome = sift(&["-c", &good, "-"], "a=1\n");
    assert_eq!(outcome.out, "5\n");
}

#[test]
fn a_double_dash_ends_the_options() {
    let outcome = sift(&["-c", "--", "-odd-name.log"], "");
    assert_eq!(outcome.code, 2);
    assert!(outcome.err.contains("-odd-name.log"), "{}", outcome.err);
}

#[test]
fn each_file_detects_its_own_format() {
    let outcome = sift(&["-c", &example("app.jsonl"), &example("access.log"), &example("service.logfmt")], "");
    assert_eq!(outcome.out, "19\n");
    assert_eq!(outcome.err, "");
}

#[test]
fn nginx_logs_can_be_queried_by_their_fields() {
    let outcome = sift_example("access.log", &["-w", "status >= 400", "-o", "table", "-F", "remote_addr,status,path"]);
    assert_eq!(
        outcome.out,
        "remote_addr  status  path\n\
         192.0.2.55      404  /wp-login.php\n\
         192.0.2.55      404  /.env\n\
         203.0.113.7     502  /api/search?q=kettle\n"
    );
}

#[test]
fn nested_fields_and_regexes_work_on_the_example() {
    let outcome = sift_example("app.jsonl", &["-w", "order.total > 15 and msg ~ '^order'", "-o", "json", "-F", "order.id"]);
    assert_eq!(outcome.out, "{\"order.id\":1001}\n{\"order.id\":1002}\n");
}

#[test]
fn array_items_match_individually() {
    let outcome = sift_example("app.jsonl", &["-c", "-w", "tags = nightly"]);
    assert_eq!(outcome.out, "1\n");
}

#[test]
fn schema_lists_the_fields() {
    let outcome = sift_example("service.logfmt", &["--schema", "-o", "logfmt"]);
    let first = outcome.out.lines().next().unwrap();
    assert_eq!(first, "field=component count=4 percent=100 types=text example=worker");
    assert_eq!(outcome.out.lines().count(), 7);
    assert!(outcome.out.contains("field=depth count=1 percent=25 types=text example=120"), "{}", outcome.out);
}

#[test]
fn stats_go_to_standard_error() {
    let outcome = sift(&["--stats", "-w", "n > 1"], SMALL);
    assert_eq!(outcome.err, "log-sift: lines=3 blank=0 bad=0 no_time=0 matched=2\n");
}

#[test]
fn windows_count_what_they_cannot_date() {
    let outcome = sift(&["--stats", "--since", "2024-01-01", "-c"], SMALL);
    assert_eq!(outcome.code, 1);
    assert!(outcome.err.contains("no_time=3"), "{}", outcome.err);
}

#[test]
fn a_file_read_from_disk_matches_standard_input() {
    let dir = std::env::temp_dir().join(format!("log-sift-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("small.jsonl");
    fs::write(&path, SMALL).unwrap();
    let outcome = sift(&["-w", "n = 2", path.to_str().unwrap()], "");
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(outcome.out, "{\"level\":\"error\",\"n\":2}\n");
}
