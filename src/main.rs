use clap::Parser;
use mbwatch::cli::{run, Cli};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let mut stdout = std::io::stdout().lock();
    match run(cli, &mut stdout).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
