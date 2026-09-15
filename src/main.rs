#![forbid(unsafe_code)]
//! `iota` binary (main.go + the signal/exit-code policy): parse the CLI (clap exits 2 on argument errors),
//! read the environment once (`Env::process`), build the multi-thread runtime, arm SIGINT/SIGTERM, run, and map the outcome — `Ok` → 0;
//! `CliError::Interrupted` → 130 (DIVERGENCES I-03); any other error → `Error: {e}` on stderr, 1 (no usage
//! block, DIVERGENCES I-04). Everything else lives in the library (`iota::cmd::run`).

use std::io::{IsTerminal as _, Write};

use clap::Parser;
use iota::app::color::ColorMode;
use iota::app::env::Env;
use iota::cmd::{Cli, CliError, io::Streams, run, signals};
use tokio_util::sync::CancellationToken;

fn main() {
    let cli = Cli::parse();
    // A `run` flag given before another verb (`iota -m hi list`) — clap cannot express this itself while
    // `-c/--config` is global, so it is checked here and reported as clap's own error (exit 2).
    if let Err(e) = cli.check_flag_placement() {
        e.exit();
    }
    // The one environment seam of the process: variables and directories, read here and injected
    // everywhere — `run` threads it through config expansion, key resolution and the listings.
    let env = Env::process();
    // `NO_COLOR` / `TERM=dumb` / a piped stdout: decided once here, read by every style helper.
    iota::app::color::init(ColorMode::detect(&env, std::io::stdout().is_terminal()));
    let mut io = Streams::process();
    // `IOTA_LOG=<path>`: the developer's diagnostic tap, installed before anything can emit.
    iota::app::diag::install_from_env(&env, &mut |w| io.warning(&w));
    let outcome = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => {
            let cancel = CancellationToken::new();
            rt.block_on(async {
                signals::install(cancel.clone());
                run(cli, env, cancel, &mut io).await
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
