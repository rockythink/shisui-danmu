use anyhow::Result;
use clap::Parser;
use shisui_danmu::{
    bilibili::AccountClient,
    config::{Cli, TerminalConfig},
    obs::{ObsConfiguration, ObsController},
    persistence::SessionJournal,
    storage::StoragePaths,
    terminal::{TerminalApp, configure_obs, interactive_login},
};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
async fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Some(shisui_danmu::config::Command::Setup { adapter }) = &cli.command {
        anyhow::ensure!(
            cli.instance.is_none()
                && cli.positional_room.is_none()
                && cli.room.is_none()
                && cli.single_line.is_none()
                && cli.show_time.is_none()
                && cli.show_name.is_none()
                && !cli.hide_name
                && cli.history_idle_seconds.is_none()
                && cli.config.is_none()
                && cli.theme.is_none()
                && !cli.login
                && !cli.logout
                && !cli.configure_obs
                && cli.replay.is_none(),
            "setup 不能混用 --instance、房间、账号、OBS、回放、配置或显示参数"
        );
        return match adapter {
            shisui_danmu::config::SetupAdapter::Pi => shisui_danmu::setup::prepare_pi().await,
        };
    }
    anyhow::ensure!(
        cli.command.is_none()
            || (cli.positional_room.is_none()
                && cli.room.is_none()
                && !cli.login
                && !cli.logout
                && !cli.configure_obs
                && cli.replay.is_none()
                && cli.config.is_none()),
        "工具/本地入口不能混用房间、登录、OBS、回放或生产配置参数"
    );
    if let Some(path) = &cli.replay {
        return shisui_danmu::terminal::replay(path).await;
    }
    if let Some(command) = &cli.command {
        let root = cli
            .instance
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("必须显式指定 --instance 绝对私有目录"))?;
        return match command {
            shisui_danmu::config::Command::Setup { .. } => unreachable!("setup 已独立分流"),
            shisui_danmu::config::Command::Local { assistant } => {
                shisui_danmu::terminal::local(root, *assistant).await
            }
            shisui_danmu::config::Command::Mcp => shisui_danmu::bridge::mcp::run(root).await,
            shisui_danmu::config::Command::Agent { operation, json } => {
                let mut args: serde_json::Value = serde_json::from_str(json)?;
                let object = args
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("参数必须为JSON对象"))?;
                anyhow::ensure!(!object.contains_key("op"), "参数不允许覆盖op");
                object.insert("op".into(), operation.clone().into());
                let value =
                    shisui_danmu::bridge::wire::call(root, serde_json::from_value(args)?).await?;
                println!("{}", value);
                Ok(())
            }
        };
    }
    let paths = StoragePaths::discover()?;
    paths.ensure()?;

    if cli.login {
        return interactive_login(AccountClient::new(paths.account_session.clone())?).await;
    }
    if cli.logout {
        AccountClient::new(paths.account_session.clone())?.sign_out()?;
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
    TerminalApp::run(config, paths.account_session, obs, journal).await
}
