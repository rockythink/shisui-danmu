use crate::storage::{StoragePaths, write_private_atomic};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
};

const PI_PACKAGE: &str = "pi-acp@0.0.33";

/// Computes the private entry without creating directories or downloading anything.
pub fn pi_adapter_path() -> Result<PathBuf> {
    Ok(pi_directory()?.join("node_modules/.bin/pi-acp"))
}

fn pi_directory() -> Result<PathBuf> {
    let paths = StoragePaths::discover()?;
    Ok(paths
        .config_file
        .parent()
        .context("无法定位应用私有配置目录")?
        .join("adapters/pi"))
}

pub(crate) fn discover_pi_adapter() -> Option<PathBuf> {
    let directory = pi_directory().ok()?;
    check_directories(&directory, false).ok()?;
    validate_adapter(&directory).ok()
}

/// Explicit preparation only: never installs Pi itself or starts either executable.
pub async fn prepare_pi() -> Result<()> {
    let directory = pi_directory()?;
    let parent = directory.parent().context("适配器缺少父目录")?;
    check_directories(parent, true)?;
    let lock_path = parent.join("pi.lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
        ensure!(
            metadata.is_file(),
            "适配器锁不是普通文件；未修改：{}",
            lock_path.display()
        );
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path).context("打开适配器准备锁失败")?;
    lock.try_lock_exclusive()
        .context("另一个 Pi 适配器准备正在运行；未修改现有安装")?;

    match fs::symlink_metadata(&directory) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir(),
                "私有适配器路径不是普通目录；未修改：{}",
                directory.display()
            );
            let entry = validate_adapter(&directory).with_context(|| {
                format!(
                    "现有目录不是已核验的 {PI_PACKAGE}；保留原内容，请手工移走后重试：{}",
                    directory.display()
                )
            })?;
            println!(
                "已就绪 {PI_PACKAGE}：{}（复用，未下载或启动）",
                entry.display()
            );
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("检查现有适配器失败"),
    }

    let staging = tempfile::Builder::new()
        .prefix(".pi-prepare-")
        .tempdir_in(parent)
        .context("创建私有适配器暂存目录失败")?;
    let install = staging.path().join("install");
    check_directories(&install, true)?;
    write_private_atomic(
        &install.join("package.json"),
        b"{\"name\":\"shisui-private-pi-adapter\",\"private\":true}\n",
    )?;
    let userconfig = staging.path().join("npmrc");
    let globalconfig = staging.path().join("global-npmrc");
    write_private_atomic(&userconfig, b"")?;
    write_private_atomic(&globalconfig, b"")?;

    println!(
        "准备 {PI_PACKAGE} 至 {}；仅私有安装，禁用安装脚本，不启动 Pi。",
        directory.display()
    );
    let mut command = tokio::process::Command::new("npm");
    // User npm configuration must not redirect this explicit private installation.
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("npm_config_")
        {
            command.env_remove(key);
        }
    }
    let status = command
        .current_dir(&install)
        .args([
            "install",
            PI_PACKAGE,
            "--global=false",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--save-exact",
            "--omit=dev",
            "--registry=https://registry.npmjs.org",
        ])
        .arg("--prefix")
        .arg(&install)
        .arg("--cache")
        .arg(staging.path().join("cache"))
        .arg("--userconfig")
        .arg(&userconfig)
        .arg("--globalconfig")
        .arg(&globalconfig)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .context("运行 npm 失败；请先安装 Node.js 20+ 与 npm；未修改现有适配器")?;
    ensure!(
        status.success(),
        "npm 准备失败（{status}）；未启用暂存内容，现有适配器不变"
    );
    validate_adapter(&install).context("暂存适配器核验失败；未启用")?;
    check_links(&install, &fs::canonicalize(&install)?)?;
    ensure!(
        fs::symlink_metadata(&directory)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "安装目标已出现；保留现有内容，未启用暂存适配器"
    );
    fs::rename(&install, &directory).context("启用私有适配器失败；未删除现有文件")?;
    println!(
        "已准备 {PI_PACKAGE}：{}\n需要另行安装原生 Pi CLI 并使 pi 位于 PATH；未修改 Pi 配置或登录，未启动助手。",
        pi_adapter_path()?.display()
    );
    Ok(())
}

// Refuse redirected application directories. Only newly created directories get private modes.
fn check_directories(path: &Path, create: bool) -> Result<()> {
    ensure!(path.is_absolute(), "适配器目录必须为绝对路径");
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                metadata.is_dir(),
                "适配器目录含链接或非目录：{}",
                current.display()
            ),
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                match builder.create(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        ensure!(
                            fs::symlink_metadata(&current)?.is_dir(),
                            "适配器目录被替换：{}",
                            current.display()
                        );
                    }
                    Err(error) => return Err(error).context("创建私有适配器目录失败"),
                }
            }
            Err(error) => return Err(error).context("检查私有适配器目录失败"),
        }
    }
    Ok(())
}

fn validate_adapter(directory: &Path) -> Result<PathBuf> {
    let package = directory.join("node_modules/pi-acp");
    check_directories(&package, false)?;
    let manifest_path = package.join("package.json");
    ensure!(
        fs::symlink_metadata(&manifest_path)?.is_file(),
        "适配器清单不是普通文件"
    );
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
    ensure!(
        manifest["name"] == "pi-acp"
            && manifest["version"] == "0.0.33"
            && manifest["bin"]["pi-acp"] == "dist/index.js",
        "适配器 name/version/bin 与已核定版本不符"
    );
    check_directories(&package.join("dist"), false)?;
    let target = package.join("dist/index.js");
    ensure!(
        fs::symlink_metadata(&target)?.is_file(),
        "适配器真实入口不是普通文件"
    );
    check_directories(&directory.join("node_modules/.bin"), false)?;
    let entry = directory.join("node_modules/.bin/pi-acp");
    ensure!(
        fs::canonicalize(&entry)? == fs::canonicalize(&target)?
            && crate::runner::settings::executable_file(&entry),
        "适配器入口无效、不可执行或指向其他位置"
    );
    Ok(entry)
}

fn check_links(directory: &Path, root: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            ensure!(
                fs::canonicalize(entry.path())?.starts_with(root),
                "适配器包含越界链接：{}",
                entry.path().display()
            );
        } else if kind.is_dir() {
            check_links(&entry.path(), root)?;
        } else {
            ensure!(
                kind.is_file(),
                "适配器包含非普通文件：{}",
                entry.path().display()
            );
        }
    }
    Ok(())
}
