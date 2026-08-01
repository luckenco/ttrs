mod cli;

fn main() -> std::process::ExitCode {
    cli::main(cli::clap::parse_env())
}
