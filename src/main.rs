use anyhow::Result;
use clap::Parser;
use shisui_danmu::{
    bilibili::AccountClient,
    config::{Cli, TerminalConfig},
    storage::StoragePaths,
    terminal::{configure_obs, interactive_login},
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
    if cli.login || cli.logout || cli.configure_obs {
        paths.ensure()?;
    }

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
    shisui_danmu::terminal::run(config, paths).await
}
