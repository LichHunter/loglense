use clap::Parser;

/// loglense
#[derive(Parser)]
#[command(name = "loglense", about = "loglense", version)]
struct Cli;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // exit() prints help/version to stdout (exit 0) and usage errors to stderr (exit 2)
    let _cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };
    println!("loglense {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}
