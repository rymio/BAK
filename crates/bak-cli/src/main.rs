use std::process::ExitCode;
use std::sync::Arc;

use bak_cli::{progress::TermProgress, Cli};
use bak_engine_rustic::RusticEngine;
use clap::Parser;

fn main() -> ExitCode {
    let cli = Cli::parse();
    bak_cli::init_logging(cli.verbose);
    // The only line that knows which engine is in use.
    let engine = RusticEngine::new(Arc::new(TermProgress::new()), bak_cli::cache_dir());
    match bak_cli::run(cli, &engine) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bak: error: {}", bak_core::redact::scrub(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}
