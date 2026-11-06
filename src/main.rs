use clap::Parser;
use session_finder::{
    cli::{Action, Cli, Request},
    render,
};
use std::io::IsTerminal;

fn run() -> anyhow::Result<()> {
    let no_arguments = std::env::args_os().len() == 1;
    let request = Request::new(
        Cli::parse(),
        no_arguments && std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
    )?;
    if request.action == Action::Browse {
        render::browse(&request)
    } else {
        render::run(&request)
    }
}

fn main() {
    if let Err(error) = run() {
        if error.chain().any(|e| {
            e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
        }) {
            return;
        }
        eprintln!(
            "Error: {}",
            session_finder::text::safe(&format!("{error:#}"), true)
        );
        std::process::exit(1);
    }
}
