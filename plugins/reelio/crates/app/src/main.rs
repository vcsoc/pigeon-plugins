//! `reelio`: the Reelio plugin process (protocol v2). Started by `sh <plugin>/run --plugin`.
//!
//! Flags: `--plugin` (serve protocol v2 on stdin/stdout), `--version`, `--uninstall` (Reelio keeps
//! no external state, so this only confirms and never touches the user's media or projects).

mod engine_bridge;
mod transport;

use std::io::{self, BufReader};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--uninstall") {
        println!("{{\"ok\":true,\"removed\":[]}}");
        return;
    }
    if args.iter().any(|a| a == "--plugin") {
        let stdin = BufReader::new(io::stdin());
        let engine = Arc::new(engine_bridge::UnconnectedEngine);
        if let Err(error) = transport::serve(stdin, io::stdout().lock(), engine) {
            eprintln!("reelio: protocol error: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("usage: reelio --plugin | --version | --uninstall");
    std::process::exit(2);
}
