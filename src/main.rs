mod lsp;
mod sqlfluff;

use clap::{Parser, Subcommand};
use lsp::FormatCommand;
use tower_lsp_server::{LspService, Server};

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
        /// SQL templater
        #[arg(short, long)]
        templater: Option<String>,
        /// Absolute path to `sqlfluff`, if not supplied
        /// `sqlfluff` should be in the PATH
        #[arg(short, long)]
        sqlfluff_path: Option<String>,
        /// `sqlfluff` command used to format documents
        #[arg(short, long, value_enum, default_value_t = FormatCommand::Fix)]
        format_command: FormatCommand,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            dialect,
            templater,
            sqlfluff_path,
            format_command,
        } => {
            let (service, socket) = LspService::new(|client| {
                lsp::Backend::new(client, dialect, templater, sqlfluff_path, format_command)
            });
            let (stdin, stdout) = (tokio::io::stdin(), tokio::io::stdout());
            Server::new(stdin, stdout, socket).serve(service).await;
        }
    }
}
