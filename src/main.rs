use std::io::{self, BufWriter, Write};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);
    let stdin = io::stdin();
    let mut stdout = BufWriter::new(io::stdout().lock());
    let code = logsift::cli::run(&args, &mut stdin.lock(), &mut stdout, &mut io::stderr(), now);
    // A reader that went away has already been dealt with inside run.
    let _ = stdout.flush();
    std::process::exit(code);
}
