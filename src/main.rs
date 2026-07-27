//! The `nostdb` binary.
//!
//! This is the one place in the workspace that touches the process streams or the process
//! status. Everything else returns a typed value, which is what lets the whole command
//! surface be driven in-process by a test.

#![forbid(unsafe_code)]

use std::io::Write;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    let class = nostdb_cli::run(&arguments, &mut out, &mut err);

    // Flush before exiting. `std::process::exit` does not unwind, so a buffered line
    // would otherwise be lost exactly when a caller most needs to read it.
    let _ = out.flush();
    let _ = err.flush();
    std::process::exit(class.code());
}
