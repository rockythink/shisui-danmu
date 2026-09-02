use anyhow::Result;
use clap::Parser;
use shisui_danmu::{
    bilibili::{AccountClient, BilibiliClient},
    config::{Cli, TerminalConfig},
    obs::{ObsConfiguration, ObsController},
    persistence::SessionJournal,
    storage::StoragePaths,
    terminal::{TerminalApp, configure_obs, interactive_login},
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("错误：{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = StoragePaths::discover()?;
    paths.ensure()?;
    let account = AccountClient::new(paths.account_session.clone())?;

    if cli.login {
        return interactive_login(account).await;
    }
    if cli.logout {
        account.sign_out()?;
        println!("已清除 TUI 独立的 B 站登录态");
        return Ok(());
    }
    if cli.configure_obs {
        return configure_obs(&paths.obs_configuration).await;
    }

    let config = TerminalConfig::load(&cli, paths.config_file.clone(), paths.themes_file.clone())?;
    let obs_configuration = ObsConfiguration::load(&paths.obs_configuration)?;
    let obs = ObsController::new(obs_configuration, paths.obs_configuration.clone());
    let journal = SessionJournal::new(paths.sessions_dir.clone());
    TerminalApp::run(
        config,
        BilibiliClient::new(paths.account_session)?,
        account,
        obs,
        journal,
    )
    .await
}
