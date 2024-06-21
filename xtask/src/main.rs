mod complement;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Complement(complement::Args),
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = match args.command {
        Command::Complement(args) => complement::main(args),
    };
    let Err(e) = result else {
        return ExitCode::SUCCESS;
    };
    // Include a leading newline because sometimes an error will occur in
    // the middle of displaying a progress indicator.
    eprintln!("\n{e:?}");
    ExitCode::FAILURE
}
