mod lsp;
mod sqlfluff;

use clap::{Parser, Subcommand};
use tower_lsp_server::{LspService, Server};
use tracing_subscriber::prelude::*;

#[derive(Parser)]
#[command(version, arg_required_else_help = true)]
struct Cli {
    /// Start lsp server
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start language server
    Serve {
        /// SQL dialect
        #[arg(short, long)]
        dialect: Option<String>,
        /// Absolute path to `sqlfluff`, if not supplied
        /// `sqlfluff` should be in the PATH
        #[arg(short, long)]
        sqlfluff_path: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_target(
                    "sqlfluff_lsp",
                    tracing_subscriber::filter::LevelFilter::WARN,
                )
                .with_default(tracing_subscriber::filter::LevelFilter::OFF),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(std::io::stderr),
        )
        .init();

    match cli.command {
        Commands::Serve {
            dialect,
            sqlfluff_path,
        } => {
            let (service, socket) =
                LspService::new(|client| lsp::Backend::new(client, dialect, sqlfluff_path));
            let (stdin, stdout) = (tokio::io::stdin(), tokio::io::stdout());
            Server::new(stdin, stdout, socket).serve(service).await;
        }
    }
}
