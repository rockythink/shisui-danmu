use super::{Bridge, Request};
use anyhow::{Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};
use uuid::Uuid;

pub const MAX_FRAME: usize = 65536;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    address: String,
    token: String,
    pid: u32,
    protocol: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    token: String,
    request: Request,
}
pub struct Server {
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
    _lock: File,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}
pub fn private_root(root: &Path) -> Result<()> {
    ensure!(root.is_absolute(), "实例目录必须为绝对路径");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root)?;
    ensure!(
        !std::fs::symlink_metadata(root)?.file_type().is_symlink(),
        "实例目录不能是符号链接"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(root)?.permissions().mode() & 0o077 == 0,
            "实例目录必须仅当前用户可访问（0700）"
        );
    }
    Ok(())
}
pub async fn serve(root: &Path, bridge: Bridge) -> Result<Server> {
    private_root(root)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(root.join("instance.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("实例目录已被占用；不停止或覆盖现用实例"))?;
    let path = root.join("instance.json");
    ensure!(
        !path.exists(),
        "发现旧实例描述文件；选择新的私有目录，不自动接管"
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let descriptor = Descriptor {
        address: listener.local_addr()?.to_string(),
        token: Uuid::new_v4().to_string(),
        pid: std::process::id(),
        protocol: 1,
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec(&descriptor)?)?;
    file.sync_all()?;
    let token = Arc::new(descriptor.token);
    let slots = Arc::new(Semaphore::new(32));
    let task = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                connection=listener.accept()=> {
                    let Ok((mut socket,_))=connection else {break};
                    let Ok(slot)=slots.clone().try_acquire_owned() else {continue};
                    let bridge=bridge.clone(); let token=token.clone();
                    clients.spawn(async move {
                        let _slot=slot;
                        let result=tokio::time::timeout(Duration::from_secs(32),async {
                            let bytes=read_frame(&mut BufReader::new(&mut socket)).await?;
                            let envelope:Envelope=serde_json::from_slice(&bytes)?;
                            ensure!(envelope.token==*token,"unauthorized");
                            let mut byte = [0_u8; 1];
                            tokio::select! {
                                result = bridge.call(envelope.request) => result,
                                _ = socket.read(&mut byte) => Err(anyhow::anyhow!("client_disconnected_or_extra_frame")),
                            }
                        }).await;
                        let response=match result {Ok(Ok(value))=>json!({"ok":true,"data":value}),Ok(Err(e))=>json!({"ok":false,"error":e.to_string()}),Err(_)=>json!({"ok":false,"error":"request_timeout"})};
                        let mut bytes=serde_json::to_vec(&response).unwrap();
                        if bytes.len() >= MAX_RESPONSE { bytes = br#"{"ok":false,"error":"response_too_large: reduce limit"}"#.to_vec(); }
                        bytes.push(b'\n');
                        let _=tokio::time::timeout(Duration::from_secs(3),socket.write_all(&bytes)).await;
                    });
                }
                _=clients.join_next(), if !clients.is_empty()=>{}
            }
        }
    });
    Ok(Server {
        task,
        path,
        _lock: lock,
    })
}
pub async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    read_limited(reader, MAX_FRAME).await
}
async fn read_limited<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        ensure!(!chunk.is_empty(), "connection_closed");
        let end = chunk.iter().position(|b| *b == b'\n');
        let n = end.map_or(chunk.len(), |i| i + 1);
        ensure!(bytes.len() + n <= limit, "frame_too_large");
        bytes.extend_from_slice(&chunk[..n]);
        reader.consume(n);
        if end.is_some() {
            return Ok(bytes);
        }
    }
}
pub async fn call(root: &Path, request: Request) -> Result<Value> {
    let path = root.join("instance.json");
    ensure!(
        !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
        "实例描述不能是符号链接"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(&path)?.permissions().mode() & 0o077 == 0,
            "实例描述权限不安全"
        );
    }
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() < 4096, "实例描述过大");
    let descriptor: Descriptor = serde_json::from_slice(&bytes)?;
    ensure!(descriptor.protocol == 1, "不支持的实例协议");
    let address: std::net::SocketAddr = descriptor.address.parse()?;
    ensure!(
        address.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "实例必须位于127.0.0.1"
    );
    let mut bytes = serde_json::to_vec(&Envelope {
        token: descriptor.token,
        request,
    })?;
    ensure!(bytes.len() < MAX_FRAME, "request_too_large");
    bytes.push(b'\n');
    let result = tokio::time::timeout(Duration::from_secs(35), async {
        let mut socket = TcpStream::connect(address).await?;
        socket.write_all(&bytes).await?;
        let bytes = read_limited(&mut BufReader::new(socket), MAX_RESPONSE).await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value["ok"] == true,
            "{}",
            value["error"].as_str().unwrap_or("invalid_response")
        );
        Ok(value["data"].clone())
    })
    .await??;
    Ok(result)
}
