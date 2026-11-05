//! Checks what only the real process can show: exit codes, pipes and standard streams.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn program() -> Command {
    Command::new(env!("CARGO_BIN_EXE_log-sift"))
}

#[test]
fn reads_standard_input_and_exits_with_the_right_code() {
    let mut child = program()
        .args(["-w", "n > 1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"n=1\nn=2\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"n=2\n");
}

#[test]
fn nothing_matching_exits_one() {
    let mut child = program().args(["-w", "n > 9"]).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"n=1\n").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(1));
}

#[test]
fn usage_errors_exit_two_and_write_to_standard_error() {
    let output = program().arg("--no-such-flag").stdin(Stdio::null()).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option"));
}

#[test]
fn a_reader_that_goes_away_is_not_an_error() {
    let mut child = program().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // Close the reading end of its output, then send more than any pipe buffer holds.
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().unwrap();
    let line = b"n=1\n".repeat(64 * 1024);
    let _ = stdin.write_all(&line);
    drop(stdin);
    let mut errors = String::new();
    child.stderr.take().unwrap().read_to_string(&mut errors).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert_eq!(errors, "");
}
