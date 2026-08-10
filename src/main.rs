mod cli;
mod config;
mod migrate;
mod paths;
mod server;
mod store;
mod store_elasticsearch;
mod store_json;
mod store_stdio;
mod types;

use clap::{CommandFactory, Parser};
#[cfg(feature = "dynamic-completion")]
use clap_complete::env::CompleteEnv;
use cli::{Cli, Commands, resolve_store};
use config::StoreRegistry;
use server::ForayServer;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    #[cfg(feature = "dynamic-completion")]
    CompleteEnv::with_factory(Cli::command).complete();

    // Without dynamic-completion, honour COMPLETE=<shell> by emitting the static script.
    #[cfg(not(feature = "dynamic-completion"))]
    if let Ok(s) = std::env::var("COMPLETE")
        && let Ok(shell) = s.parse::<clap_complete::Shell>()
    {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    let cli = Cli::parse();

    let registry = StoreRegistry::load()?;

    if matches!(cli.command, Commands::Serve) {
        let version = env!("CARGO_PKG_VERSION");
        let git = env!("FORAY_GIT_DESCRIBE");
        let home = std::env::var("FORAY_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| format!(" FORAY_HOME={v}"))
            .unwrap_or_default();
        eprintln!("foray {version} ({git}){home}");

        let server = ForayServer::new(registry);
        let transport = rmcp::transport::io::stdio();
        let service = rmcp::serve_server(server, transport).await?;
        service.waiting().await?;
        return Ok(());
    }

    if let Commands::Completions { shell } = cli.command {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    let store = resolve_store(&registry, cli.store.as_deref())?;
    cli::run(&cli, store).await?;
    Ok(())
}
