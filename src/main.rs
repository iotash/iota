#![forbid(unsafe_code)]
//! `iota` binary (main.go + the signal/exit-code policy): parse the CLI (clap exits 2 on argument errors),
//! resolve `HostDirs`, build the multi-thread runtime, arm SIGINT/SIGTERM, run, and map the outcome — `Ok` → 0;
//! `CliError::Interrupted` → 130 (DIVERGENCES I-03); any other error → `Error: {e}` on stderr, 1 (no usage
//! block, DIVERGENCES I-04). Everything else lives in the library (`iota::cmd::run`).

use std::{io::Write, sync::Arc};

use clap::Parser;
use iota::app::HostDirs;
use iota::cmd::{Cli, CliError, io::Streams, run, signals};
use iota::color::ColorMode;
use iota::vars::{EnvSource, ProcessEnv};
use tokio_util::sync::CancellationToken;

fn main() {
    let cli = Cli::parse();
    // A `run` flag given before another verb (`iota -m hi list`) — clap cannot express this itself while
    // `-c/--config` is global, so it is checked here and reported as clap's own error (exit 2).
    if let Err(e) = cli.check_flag_placement() {
        e.exit();
    }
    let dirs = HostDirs::from_env();
    // `NO_COLOR` / `TERM=dumb` / a piped stdout: decided once here, read by every style helper.
    iota::color::init(ColorMode::from_process());
    let mut io = Streams::process();
    let outcome = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => {
            let cancel = CancellationToken::new();
            // The one environment seam of the process: `run` threads it through config expansion, key
            // resolution and `-l` (`Arc` because it outlives this scope).
            let env: Arc<dyn EnvSource> = Arc::new(ProcessEnv);
            rt.block_on(async {
                signals::install(cancel.clone());
                run(cli, dirs, env, cancel, &mut io).await
            })
        }
        Err(e) => Err(CliError::Io(e)),
    };
    let code = match outcome {
        Ok(()) => 0,
        Err(CliError::Interrupted) => 130,
        Err(e) => {
            let _ = writeln!(io.stderr, "Error: {e}");
            1
        }
    };
    std::process::exit(code);
}
