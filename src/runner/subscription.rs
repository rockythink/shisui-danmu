//! Official public-client Sign in with ChatGPT. No host credentials or billing fallback.
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use reqwest::{Client, Response, header::HeaderValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use url::Url;

const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const INITIAL_CLIENT: &str = "dynamic_agent_client";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DIRECT: &str = "chatgpt.tokens.use.direct";
const MAX_FILE: usize = 256 * 1024;
const MAX_HTTP: usize = 96 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountInfo {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub signed_in: bool,
    pub plan_enabled: bool,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum SubscriptionError {
    #[error("订阅HTTP错误：{0}")]
    Http(u16),
    #[error("订阅网络错误：{0}")]
    Transport(&'static str),
    #[error("订阅协议错误：{0}")]
    Protocol(&'static str),
    #[error("订阅配置错误：{0}")]
    Configuration(&'static str),
    #[error("未授权使用ChatGPT订阅额度")]
    ScopeDisabled,
    #[error("用户未同意登录授权")]
    AccessDenied,
    #[error("需要重新登录：{0}")]
    Refresh(&'static str),
}
type OAuthResult<T> = std::result::Result<T, SubscriptionError>;
fn storage_error(_: anyhow::Error) -> SubscriptionError {
    SubscriptionError::Configuration("私有订阅存储不可用")
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn safe_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn token_text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16384 && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}
fn random_secret() -> String {
    // UUID v4 is supplied by the OS CSPRNG; combine two for 244 random bits.
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

// Only these private disk records are serializable, never the public runtime handles.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    format: u8,
    host_id: String,
    accounts: Vec<Registration>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    id: String,
    client_id: String,
    subject: String,
    email: Option<String>,
    scopes: Vec<String>,
    revision: u64,
    tokens: Option<Tokens>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_at: u64,
    earliest_refresh_at: Option<u64>,
}
impl Registration {
    fn info(&self) -> AccountInfo {
        AccountInfo {
            id: self.id.clone(),
            label: format!(
                "{} · {}",
                self.email.as_deref().unwrap_or("ChatGPT账号"),
                self.id
            ),
            email: self.email.clone(),
            signed_in: self.tokens.is_some(),
            plan_enabled: self.tokens.is_some() && self.scopes.iter().any(|s| s == DIRECT),
        }
    }
}
struct Paths {
    root: PathBuf,
    file: PathBuf,
    lock: PathBuf,
}
fn paths(config: &Path, create: bool) -> Result<Paths> {
    let parent = config
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("订阅配置父目录不存在"))?;
    let meta = std::fs::metadata(&parent)?;
    ensure!(meta.is_dir(), "订阅配置父路径不是目录");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
            "订阅配置父目录不可由其他用户写入"
        );
    }
    let root = parent.join("assistant-secrets");
    match std::fs::symlink_metadata(&root) {
        Ok(meta) => {
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "订阅私有目录不能是符号链接"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
                    "订阅私有目录须为本人0700目录"
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if create {
                crate::bridge::wire::private_root(&root)?;
            }
        }
        Err(_) => anyhow::bail!("订阅私有目录检查失败"),
    }
    Ok(Paths {
        file: root.join("chatgpt.json"),
        lock: root.join("chatgpt.lock"),
        root,
    })
}
fn check_meta(meta: &std::fs::Metadata) -> Result<()> {
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "订阅文件必须为普通文件"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.mode() & 0o077 == 0,
            "订阅文件须仅本人可访问且无硬链接"
        );
    }
    Ok(())
}
fn open_private(path: &Path, create: bool) -> Result<File> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => check_meta(&meta)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => anyhow::bail!("订阅文件检查失败"),
    }
    let mut opts = OpenOptions::new();
    opts.read(true).write(create).create(create).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts
        .open(path)
        .map_err(|_| anyhow::anyhow!("订阅文件打开失败"))?;
    check_meta(&file.metadata()?)?;
    Ok(file)
}
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn lock_sync(p: &Paths) -> Result<Lock> {
    let file = open_private(&p.lock, true)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| anyhow::anyhow!("订阅存储正被其他进程使用"))?;
    Ok(Lock(file))
}
async fn lock_async(p: &Paths) -> OAuthResult<Lock> {
    let file = open_private(&p.lock, true).map_err(storage_error)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Lock(file)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(SubscriptionError::Configuration("订阅锁不可用")),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(SubscriptionError::Configuration("订阅锁等待超时"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
fn read_store(p: &Paths) -> Result<Option<Store>> {
    if !p.root.exists() {
        return Ok(None);
    }
    let file = match open_private(&p.file, false) {
        Ok(file) => file,
        Err(e) => {
            if matches!(&std::fs::symlink_metadata(&p.file), Err(io) if io.kind() == std::io::ErrorKind::NotFound)
            {
                return Ok(None);
            }
            return Err(e);
        }
    };
    ensure!(file.metadata()?.len() <= MAX_FILE as u64, "订阅文件超限");
    let mut bytes = Vec::new();
    file.take(MAX_FILE as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_FILE, "订阅文件超限");
    let store: Store =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("订阅文件损坏，未覆盖"))?;
    validate_store(&store)?;
    Ok(Some(store))
}
fn validate_store(s: &Store) -> Result<()> {
    let host = s
        .host_id
        .strip_prefix("urn:uuid:")
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    ensure!(
        s.format == 1 && host.is_some_and(|u| u.get_version_num() == 4) && s.accounts.len() <= 32,
        "订阅存储格式错误"
    );
    let mut ids = std::collections::BTreeSet::new();
    let mut bindings = std::collections::BTreeSet::new();
    for a in &s.accounts {
        ensure!(
            uuid::Uuid::parse_str(&a.id).is_ok() && ids.insert(&a.id),
            "订阅注册ID错误"
        );
        ensure!(
            safe_text(&a.client_id, 256)
                && a.client_id != INITIAL_CLIENT
                && safe_text(&a.subject, 512)
                && bindings.insert((&a.client_id, &a.subject)),
            "订阅注册绑定错误"
        );
        ensure!(
            a.email.as_deref().is_none_or(|s| safe_text(s, 320))
                && a.scopes.len() <= 32
                && a.scopes
                    .iter()
                    .all(|s| safe_text(s, 128) && !s.contains(char::is_whitespace)),
            "订阅资料格式错误"
        );
        if let Some(t) = &a.tokens {
            ensure!(
                token_text(&t.access_token)
                    && token_text(&t.refresh_token)
                    && token_text(&t.id_token)
                    && t.expires_at > 0,
                "订阅凭据格式错误"
            );
        }
    }
    Ok(())
}
fn write_store(p: &Paths, s: &Store) -> Result<()> {
    validate_store(s)?;
    // Check destination again before replacement; never silently replace an unsafe path.
    match std::fs::symlink_metadata(&p.file) {
        Ok(meta) => check_meta(&meta)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => anyhow::bail!("订阅目标文件检查失败"),
    }
    let bytes = serde_json::to_vec(s).map_err(|_| anyhow::anyhow!("订阅凭据编码失败"))?;
    ensure!(bytes.len() <= MAX_FILE, "订阅文件超限");
    crate::storage::write_private_atomic(&p.file, &bytes)
}
pub(crate) fn load_accounts(config_path: &Path) -> Result<Vec<AccountInfo>> {
    Ok(read_store(&paths(config_path, false)?)?
        .map(|s| s.accounts.iter().map(Registration::info).collect())
        .unwrap_or_default())
}

#[derive(Clone)]
struct Endpoints {
    authorize: String,
    discovery: String,
    token: String,
    issuer: String,
    test_origin: Option<String>,
}

// Explicit process-local seam compiled only for isolated tests/debug QA.
// Production has no environment or configuration endpoint override.
#[cfg(any(test, feature = "test-support"))]
static TEST_ENDPOINTS: std::sync::OnceLock<Endpoints> = std::sync::OnceLock::new();
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn install_test_oauth_origin(origin: &str) -> Result<()> {
    let parsed = Url::parse(origin)?;
    ensure!(
        parsed.scheme() == "http"
            && parsed.host_str() == Some("127.0.0.1")
            && parsed.path() == "/"
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none(),
        "OAuth测试端点须为loopback origin"
    );
    let origin = parsed.origin().ascii_serialization();
    TEST_ENDPOINTS
        .set(Endpoints {
            authorize: format!("{origin}/authorize"),
            discovery: format!("{origin}/discovery"),
            token: format!("{origin}/token"),
            issuer: ISSUER.into(),
            test_origin: Some(origin),
        })
        .map_err(|_| anyhow::anyhow!("OAuth测试端点已安装"))
}
impl Endpoints {
    fn official() -> Self {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(endpoints) = TEST_ENDPOINTS.get() {
            return endpoints.clone();
        }
        Self {
            authorize: format!("{ISSUER}/api/accounts/authorize"),
            discovery: format!("{ISSUER}/.well-known/openid-configuration"),
            token: format!("{ISSUER}/api/accounts/oauth/token"),
            issuer: ISSUER.into(),
            test_origin: None,
        }
    }
    fn allowed(&self, url: &str) -> bool {
        let Ok(u) = Url::parse(url) else {
            return false;
        };
        if !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
        {
            return false;
        }
        #[cfg(any(test, feature = "test-support"))]
        if let Some(origin) = &self.test_origin {
            return u.origin().ascii_serialization() == *origin
                && u.host_str() == Some("127.0.0.1");
        }
        u.scheme() == "https"
            && u.host_str() == Some("auth.openai.com")
            && u.port_or_known_default() == Some(443)
    }
}
fn client(endpoints: &Endpoints) -> OAuthResult<Client> {
    let builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30));
    let builder = if endpoints.test_origin.is_some() {
        builder.no_proxy()
    } else {
        builder
    };
    builder
        .build()
        .map_err(|_| SubscriptionError::Configuration("无法构造订阅客户端"))
}
async fn bounded(mut response: Response) -> OAuthResult<(u16, Vec<u8>)> {
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|n| n > MAX_HTTP as u64)
    {
        return Err(SubscriptionError::Protocol("响应超限"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SubscriptionError::Transport("响应中断"))?
    {
        if chunk.len() > MAX_HTTP - body.len() {
            return Err(SubscriptionError::Protocol("响应超限"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}
#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    token_endpoint: String,
    revocation_endpoint: String,
}
async fn discovery(client: &Client, ep: &Endpoints) -> OAuthResult<Discovery> {
    let response = client
        .get(&ep.discovery)
        .send()
        .await
        .map_err(|_| SubscriptionError::Transport("身份发现失败"))?;
    let (status, bytes) = bounded(response).await?;
    if status != 200 {
        return Err(SubscriptionError::Http(status));
    }
    let d: Discovery = serde_json::from_slice(&bytes)
        .map_err(|_| SubscriptionError::Protocol("身份发现格式错误"))?;
    if d.issuer != ep.issuer
        || d.token_endpoint != ep.token
        || !ep.allowed(&d.jwks_uri)
        || !ep.allowed(&d.revocation_endpoint)
    {
        return Err(SubscriptionError::Protocol("身份发现端点不匹配"));
    }
    Ok(d)
}
async fn jwks(client: &Client, ep: &Endpoints) -> OAuthResult<JwkSet> {
    let d = discovery(client, ep).await?;
    let response = client
        .get(&d.jwks_uri)
        .send()
        .await
        .map_err(|_| SubscriptionError::Transport("签名密钥读取失败"))?;
    let (status, bytes) = bounded(response).await?;
    if status != 200 {
        return Err(SubscriptionError::Http(status));
    }
    let keys: JwkSet = serde_json::from_slice(&bytes)
        .map_err(|_| SubscriptionError::Protocol("签名密钥格式错误"))?;
    if keys.keys.is_empty() || keys.keys.len() > 32 {
        return Err(SubscriptionError::Protocol("签名密钥数量错误"));
    }
    Ok(keys)
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    aud: serde_json::Value,
    nonce: Option<String>,
    email: Option<String>,
    client_id: Option<String>,
    scope: Option<String>,
    exp: u64,
    azp: Option<String>,
}
fn verify(token: &str, keys: &JwkSet, issuer: &str, audience: &str) -> OAuthResult<Claims> {
    let header = decode_header(token).map_err(|_| SubscriptionError::Protocol("JWT格式错误"))?;
    if !matches!(
        header.alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384
    ) {
        return Err(SubscriptionError::Protocol("JWT签名算法不允许"));
    }
    let kid = header
        .kid
        .ok_or(SubscriptionError::Protocol("JWT缺少签名标识"))?;
    let mut matches = keys
        .keys
        .iter()
        .filter(|k| k.common.key_id.as_deref() == Some(&kid));
    let matched = matches
        .next()
        .ok_or(SubscriptionError::Protocol("JWT签名标识不唯一"))?;
    if matches.next().is_some() {
        return Err(SubscriptionError::Protocol("JWT签名标识不唯一"));
    }
    let key =
        DecodingKey::from_jwk(matched).map_err(|_| SubscriptionError::Protocol("JWT密钥错误"))?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.leeway = 0;
    validation.validate_nbf = true;
    let claims = decode::<Claims>(token, &key, &validation)
        .map_err(|_| SubscriptionError::Protocol("JWT身份校验失败"))?
        .claims;
    if claims.aud.as_array().is_some_and(|a| a.len() > 1) && claims.azp.as_deref() != Some(audience)
    {
        return Err(SubscriptionError::Protocol("JWT多受众缺少绑定"));
    }
    if !safe_text(&claims.sub, 512) || claims.email.as_deref().is_some_and(|s| !safe_text(s, 320)) {
        return Err(SubscriptionError::Protocol("JWT资料错误"));
    }
    Ok(claims)
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: String,
    earliest_refresh_at: Option<u64>,
}
fn refresh_code(bytes: &[u8]) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    match v.get("error")?.as_str()? {
        "invalid_grant" => Some("invalid_grant"),
        "invalid_refresh_token" => Some("invalid_refresh_token"),
        "token_expired" => Some("token_expired"),
        "refresh_token_expired" => Some("refresh_token_expired"),
        "refresh_token_invalidated" => Some("refresh_token_invalidated"),
        "refresh_token_reused" => Some("refresh_token_reused"),
        "invalid_client" => Some("invalid_client"),
        _ => None,
    }
}
async fn exchange(
    client: &Client,
    ep: &Endpoints,
    form: &[(&str, &str)],
) -> OAuthResult<TokenResponse> {
    let response = client
        .post(&ep.token)
        .form(form)
        .send()
        .await
        .map_err(|_| SubscriptionError::Transport("凭据交换失败"))?;
    let (status, bytes) = bounded(response).await?;
    if status != 200 {
        return Err(if let Some(code) = refresh_code(&bytes) {
            SubscriptionError::Refresh(code)
        } else {
            SubscriptionError::Http(status)
        });
    }
    let t: TokenResponse = serde_json::from_slice(&bytes)
        .map_err(|_| SubscriptionError::Protocol("凭据响应格式错误"))?;
    if t.token_type != "Bearer"
        || t.expires_in == 0
        || t.expires_in > 86400
        || !token_text(&t.access_token)
        || !token_text(&t.refresh_token)
        || t.id_token.as_deref().is_some_and(|t| !token_text(t))
        || t.scope.len() > 4096
    {
        return Err(SubscriptionError::Protocol("凭据响应字段错误"));
    }
    Ok(t)
}
struct Attempt {
    host_id: String,
    id: String,
    revision: Option<u64>,
    client_id: String,
    subject: Option<String>,
    nonce: String,
    state: String,
    verifier: String,
    redirect: String,
}
pub(crate) struct PendingLogin {
    host_id: String,
    registration: Registration,
    previous_revision: Option<u64>,
}
pub(crate) struct Login {
    url: String,
    task: Option<JoinHandle<OAuthResult<PendingLogin>>>,
}
impl Login {
    pub(crate) fn url(&self) -> &str {
        &self.url
    }
    pub(crate) async fn finish(mut self) -> OAuthResult<PendingLogin> {
        // Keep ownership while waiting so cancellation of finish also aborts the task.
        let result = self
            .task
            .as_mut()
            .ok_or(SubscriptionError::Configuration("登录已结束"))?
            .await
            .map_err(|_| SubscriptionError::Transport("登录任务已取消"))?;
        self.task.take();
        result
    }
}
impl Drop for Login {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
pub(crate) fn begin_login(config_path: &Path, account_id: Option<&str>) -> Result<Login> {
    begin_login_with(config_path, account_id, Endpoints::official())
}
fn begin_login_with(config_path: &Path, account_id: Option<&str>, ep: Endpoints) -> Result<Login> {
    ensure!(
        tokio::runtime::Handle::try_current().is_ok(),
        "订阅登录需要应用异步运行时"
    );
    let p = paths(config_path, true)?;
    let _lock = lock_sync(&p)?;
    let existing = read_store(&p)?;
    let fresh = existing.is_none();
    let store = existing.unwrap_or_else(|| Store {
        format: 1,
        host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
        accounts: Vec::new(),
    });
    if fresh {
        write_store(&p, &store)?;
    }
    let selected = match account_id {
        Some(id) => Some(
            store
                .accounts
                .iter()
                .find(|a| a.id == id)
                .ok_or_else(|| anyhow::anyhow!("订阅账号不存在"))?,
        ),
        None => {
            ensure!(store.accounts.len() < 32, "订阅注册数量已达上限");
            None
        }
    };
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let redirect = format!(
        "http://127.0.0.1:{}/auth/callback",
        listener.local_addr()?.port()
    );
    let state = random_secret();
    let nonce = random_secret();
    let verifier = random_secret();
    let mut url = Url::parse(&ep.authorize)?;
    let client_id = selected
        .as_ref()
        .map(|a| a.client_id.clone())
        .unwrap_or_else(|| INITIAL_CLIENT.into());
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &client_id)
            .append_pair("ext_agent_host_id", &store.host_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect)
            .append_pair("resource", RESOURCE)
            .append_pair("scope", SCOPE)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair(
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            );
        if let Some(a) = &selected {
            if let Some(t) = &a.tokens {
                q.append_pair("id_token_hint", &t.id_token);
            }
            if let Some(email) = &a.email {
                q.append_pair("login_hint", email);
            }
            if !a.scopes.iter().any(|s| s == DIRECT) {
                q.append_pair("prompt", "consent");
            }
        } else {
            q.append_pair("agent_name_hint", "shisui-danmu");
        }
    }
    let attempt = Attempt {
        host_id: store.host_id.clone(),
        id: selected
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        revision: selected.as_ref().map(|a| a.revision),
        subject: selected.as_ref().map(|a| a.subject.clone()),
        client_id,
        nonce,
        state,
        verifier,
        redirect,
    };
    let listener = TcpListener::from_std(listener)?;
    let http = client(&ep)?;
    let task = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(300),
            login_flow(listener, attempt, http, ep),
        )
        .await
        .map_err(|_| SubscriptionError::Transport("登录等待超时"))?
    });
    Ok(Login {
        url: url.into(),
        task: Some(task),
    })
}
fn callback(target: &str, host: &str, a: &Attempt) -> OAuthResult<(String, String)> {
    let base =
        Url::parse(&a.redirect).map_err(|_| SubscriptionError::Configuration("回调地址错误"))?;
    if host != &base[url::Position::BeforeHost..url::Position::AfterPort]
        || !target.starts_with("/auth/callback?")
    {
        return Err(SubscriptionError::Protocol("回调地址不匹配"));
    }
    let url = Url::parse(&format!("http://{host}{target}"))
        .map_err(|_| SubscriptionError::Protocol("回调地址错误"))?;
    if url.path() != "/auth/callback" || url.fragment().is_some() {
        return Err(SubscriptionError::Protocol("回调路径不匹配"));
    }
    let mut fields = BTreeMap::new();
    for (k, v) in url.query_pairs() {
        if fields.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(SubscriptionError::Protocol("回调参数重复"));
        }
    }
    if fields.get("state") != Some(&a.state) {
        return Err(SubscriptionError::Protocol("回调state不匹配"));
    }
    if let Some(error) = fields.get("error") {
        return Err(if error == "access_denied" {
            SubscriptionError::AccessDenied
        } else {
            SubscriptionError::Protocol("授权失败")
        });
    }
    let client = if a.client_id == INITIAL_CLIENT {
        fields
            .get("client_id")
            .cloned()
            .ok_or(SubscriptionError::Protocol("注册未返回client ID"))?
    } else {
        if fields.get("client_id").is_some_and(|c| c != &a.client_id) {
            return Err(SubscriptionError::Protocol("返回client ID不匹配"));
        }
        a.client_id.clone()
    };
    let code = fields
        .get("code")
        .cloned()
        .ok_or(SubscriptionError::Protocol("回调缺少code"))?;
    if client == INITIAL_CLIENT || !safe_text(&client, 256) || !token_text(&code) {
        return Err(SubscriptionError::Protocol("回调凭据格式错误"));
    }
    Ok((code, client))
}
async fn login_flow(
    listener: TcpListener,
    a: Attempt,
    client: Client,
    ep: Endpoints,
) -> OAuthResult<PendingLogin> {
    let (mut socket, peer) = listener
        .accept()
        .await
        .map_err(|_| SubscriptionError::Transport("回调接收失败"))?;
    if !peer.ip().is_loopback() {
        return Err(SubscriptionError::Protocol("回调来源不允许"));
    }
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let n = socket
                .read(&mut chunk)
                .await
                .map_err(|_| SubscriptionError::Transport("回调读取失败"))?;
            if n == 0 {
                return Err(SubscriptionError::Protocol("回调请求不完整"));
            }
            bytes.extend_from_slice(&chunk[..n]);
            if bytes.len() > 16384 {
                return Err(SubscriptionError::Protocol("回调请求超限"));
            }
            if bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(bytes).map_err(|_| SubscriptionError::Protocol("回调请求格式错误"))
    })
    .await
    .map_err(|_| SubscriptionError::Transport("回调读取超时"))??;
    let parsed = (|| {
        let mut lines = request.split("\r\n");
        let first: Vec<_> = lines
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        if first.len() != 3 || first[0] != "GET" || first[2] != "HTTP/1.1" {
            return Err(SubscriptionError::Protocol("回调方法错误"));
        }
        let mut host = None;
        for line in lines.take_while(|l| !l.is_empty()) {
            let (name, value) = line
                .split_once(':')
                .ok_or(SubscriptionError::Protocol("回调头格式错误"))?;
            if name.eq_ignore_ascii_case("host") && host.replace(value.trim()).is_some() {
                return Err(SubscriptionError::Protocol("回调Host重复"));
            }
        }
        callback(
            first[1],
            host.ok_or(SubscriptionError::Protocol("回调缺少Host"))?,
            &a,
        )
    })();
    let (status, text) = if parsed.is_ok() {
        ("200 OK", "Return to shisui-danmu to finish login")
    } else {
        ("400 Bad Request", "Login rejected")
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = socket.write_all(reply.as_bytes()).await;
    drop(socket);
    drop(listener);
    let (code, issued) = parsed?;
    let t = exchange(
        &client,
        &ep,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &issued),
            ("code", &code),
            ("code_verifier", &a.verifier),
            ("redirect_uri", &a.redirect),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    let keys = jwks(&client, &ep).await?;
    let id = t
        .id_token
        .as_deref()
        .ok_or(SubscriptionError::Protocol("缺少ID token"))?;
    let claims = verify(id, &keys, &ep.issuer, &issued)?;
    if claims.nonce.as_deref() != Some(&a.nonce)
        || claims.azp.as_deref().is_some_and(|c| c != issued)
        || a.subject.as_ref().is_some_and(|s| s != &claims.sub)
    {
        return Err(SubscriptionError::Protocol("登录身份绑定不匹配"));
    }
    let (tokens, scopes) = validate_tokens(t, &keys, &ep, &issued, &claims.sub, None)?;
    Ok(PendingLogin {
        host_id: a.host_id,
        previous_revision: a.revision,
        registration: Registration {
            id: a.id,
            client_id: issued,
            subject: claims.sub,
            email: claims.email,
            scopes,
            revision: a.revision.unwrap_or(0).saturating_add(1),
            tokens: Some(tokens),
        },
    })
}
fn validate_tokens(
    t: TokenResponse,
    keys: &JwkSet,
    ep: &Endpoints,
    issued: &str,
    subject: &str,
    previous_id: Option<&str>,
) -> OAuthResult<(Tokens, Vec<String>)> {
    let access = verify(&t.access_token, keys, &ep.issuer, RESOURCE)?;
    let scopes: Vec<String> = t.scope.split_whitespace().map(str::to_owned).collect();
    if scopes.len() > 32
        || scopes.iter().any(|s| !safe_text(s, 128))
        || access.sub != subject
        || access.client_id.as_deref() != Some(issued)
    {
        return Err(SubscriptionError::Protocol("访问凭据绑定不匹配"));
    }
    let access_scopes: std::collections::BTreeSet<_> = access
        .scope
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if !scopes.iter().all(|s| access_scopes.contains(s.as_str())) {
        return Err(SubscriptionError::Protocol("访问凭据scope不匹配"));
    }
    let id_token = t
        .id_token
        .or_else(|| previous_id.map(str::to_owned))
        .ok_or(SubscriptionError::Protocol("缺少ID token"))?;
    let expires_at = now().saturating_add(t.expires_in).min(access.exp);
    Ok((
        Tokens {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            id_token,
            expires_at,
            earliest_refresh_at: t.earliest_refresh_at,
        },
        scopes,
    ))
}
pub(crate) fn complete_login(config_path: &Path, pending: PendingLogin) -> Result<AccountInfo> {
    let p = paths(config_path, false)?;
    let _lock = lock_sync(&p)?;
    let mut store = read_store(&p)?.ok_or_else(|| anyhow::anyhow!("订阅host记录已丢失"))?;
    ensure!(store.host_id == pending.host_id, "登录host已改变");
    let a = pending.registration;
    let info = a.info();
    if let Some(index) = store.accounts.iter().position(|old| old.id == a.id) {
        let old = &store.accounts[index];
        ensure!(
            Some(old.revision) == pending.previous_revision
                && old.client_id == a.client_id
                && old.subject == a.subject,
            "登录期间注册已改变，请重新登录"
        );
        store.accounts[index] = a;
    } else {
        ensure!(pending.previous_revision.is_none(), "登录期间注册已丢失");
        store.accounts.push(a);
    }
    write_store(&p, &store)?;
    Ok(info)
}

pub(crate) struct Session {
    config: PathBuf,
    host_id: String,
    account_id: String,
    client_id: String,
    subject: String,
    endpoints: Endpoints,
    client: Client,
}
pub(crate) fn session(config_path: &Path, account_id: &str) -> Result<Session> {
    session_with(config_path, account_id, Endpoints::official())
}
fn session_with(config: &Path, id: &str, ep: Endpoints) -> Result<Session> {
    let store =
        read_store(&paths(config, false)?)?.ok_or_else(|| anyhow::anyhow!("尚未登录ChatGPT"))?;
    let a = store
        .accounts
        .iter()
        .find(|a| a.id == id)
        .ok_or_else(|| anyhow::anyhow!("订阅账号不存在"))?;
    ensure!(a.tokens.is_some(), "订阅账号已退出，请重新登录");
    ensure!(
        a.scopes.iter().any(|s| s == DIRECT) && a.scopes.iter().any(|s| s == "resource.invoke"),
        "未授权使用ChatGPT订阅额度"
    );
    Ok(Session {
        config: config.to_owned(),
        host_id: store.host_id,
        account_id: a.id.clone(),
        client_id: a.client_id.clone(),
        subject: a.subject.clone(),
        client: client(&ep)?,
        endpoints: ep,
    })
}
impl Session {
    /// Nonsecret local registration ID; callers must match the selected connection.
    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }

    pub(crate) async fn header(&mut self) -> OAuthResult<HeaderValue> {
        let p = paths(&self.config, false).map_err(storage_error)?;
        let _lock = lock_async(&p).await?;
        let mut store = read_store(&p)
            .map_err(storage_error)?
            .ok_or(SubscriptionError::Configuration("订阅记录缺失"))?;
        if store.host_id != self.host_id {
            return Err(SubscriptionError::Configuration("订阅host已改变"));
        }
        let a = store
            .accounts
            .iter_mut()
            .find(|a| a.id == self.account_id)
            .ok_or(SubscriptionError::Configuration("订阅注册缺失"))?;
        if a.client_id != self.client_id || a.subject != self.subject {
            return Err(SubscriptionError::Configuration("订阅注册绑定已改变"));
        }
        if !a.scopes.iter().any(|s| s == DIRECT) || !a.scopes.iter().any(|s| s == "resource.invoke")
        {
            return Err(SubscriptionError::ScopeDisabled);
        }
        let t = a
            .tokens
            .as_ref()
            .ok_or(SubscriptionError::Configuration("订阅账号已退出"))?;
        let refresh = t.expires_at <= now().saturating_add(60)
            && t.earliest_refresh_at.is_none_or(|time| time <= now());
        if refresh {
            // Fetch signing keys before consuming a rotating refresh token. Once
            // exchange completes, validation and durable replacement have no await.
            let keys = jwks(&self.client, &self.endpoints).await?;
            let response = exchange(
                &self.client,
                &self.endpoints,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", &a.client_id),
                    ("refresh_token", &t.refresh_token),
                    ("resource", RESOURCE),
                ],
            )
            .await;
            let response = match response {
                Ok(t) => t,
                Err(SubscriptionError::Refresh(code)) if code != "invalid_client" => {
                    a.tokens = None;
                    a.revision = a.revision.saturating_add(1);
                    write_store(&p, &store).map_err(storage_error)?;
                    return Err(SubscriptionError::Refresh(code));
                }
                Err(e) => return Err(e),
            };
            if let Some(id) = &response.id_token {
                let claims = verify(id, &keys, &self.endpoints.issuer, &a.client_id)?;
                if claims.sub != a.subject
                    || claims.azp.as_deref().is_some_and(|c| c != a.client_id)
                {
                    return Err(SubscriptionError::Protocol("刷新身份不匹配"));
                }
            }
            let (tokens, scopes) = validate_tokens(
                response,
                &keys,
                &self.endpoints,
                &a.client_id,
                &a.subject,
                Some(&t.id_token),
            )?;
            a.tokens = Some(tokens);
            a.scopes = scopes;
            a.revision = a.revision.saturating_add(1);
            write_store(&p, &store).map_err(storage_error)?;
        }
        let a = store
            .accounts
            .iter()
            .find(|a| a.id == self.account_id)
            .ok_or(SubscriptionError::Configuration("订阅注册缺失"))?;
        if !a.scopes.iter().any(|s| s == DIRECT) || !a.scopes.iter().any(|s| s == "resource.invoke")
        {
            return Err(SubscriptionError::ScopeDisabled);
        }
        let t = a
            .tokens
            .as_ref()
            .ok_or(SubscriptionError::Configuration("订阅账号已退出"))?;
        if t.expires_at <= now() {
            return Err(SubscriptionError::Configuration(
                "访问凭据已过期且尚不能刷新",
            ));
        }
        let mut header = HeaderValue::from_str(&format!("Bearer {}", t.access_token))
            .map_err(|_| SubscriptionError::Protocol("认证头格式错误"))?;
        header.set_sensitive(true);
        Ok(header)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SignOutOutcome {
    pub remote_revoked: bool,
}
pub(crate) async fn sign_out(config_path: &Path, account_id: &str) -> OAuthResult<SignOutOutcome> {
    sign_out_with(config_path, account_id, Endpoints::official()).await
}
async fn sign_out_with(config: &Path, id: &str, ep: Endpoints) -> OAuthResult<SignOutOutcome> {
    let p = paths(config, false).map_err(storage_error)?;
    let _lock = lock_async(&p).await?;
    let mut store = read_store(&p)
        .map_err(storage_error)?
        .ok_or(SubscriptionError::Configuration("订阅注册缺失"))?;
    let a = store
        .accounts
        .iter_mut()
        .find(|a| a.id == id)
        .ok_or(SubscriptionError::Configuration("订阅账号不存在"))?;
    // Commit local signout before any network await. Cancellation can leave remote
    // revocation unconfirmed, but cannot resurrect locally usable credentials.
    let tokens = a.tokens.take();
    let client_id = a.client_id.clone();
    a.revision = a.revision.saturating_add(1);
    write_store(&p, &store).map_err(storage_error)?;
    drop(_lock);
    let mut remote_revoked = false;
    if let Some(t) = tokens
        && let Ok(http) = client(&ep)
        && let Ok(d) = discovery(&http, &ep).await
        && let Ok(response) = http
            .post(&d.revocation_endpoint)
            .form(&[
                ("token", t.refresh_token.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", client_id.as_str()),
            ])
            .send()
            .await
        && let Ok((status, bytes)) = bounded(response).await
    {
        remote_revoked = status == 200 && bytes.is_empty();
    }
    Ok(SignOutOutcome { remote_revoked })
}

#[cfg(test)]
#[path = "subscription_tests.rs"]
mod tests;

#[cfg(any(test, feature = "test-support"))]
#[path = "subscription_fixtures.rs"]
mod fixtures;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use fixtures::{
    FIXTURE_ACCOUNT, fixture_discovery, fixture_jwks, fixture_login_token_response,
    fixture_token_response,
};
#[cfg(test)]
use fixtures::{keys, signed, token_response};
#[cfg(all(test, feature = "tui"))]
pub(crate) use tests::fixture_finish_login;
#[cfg(test)]
pub(crate) use tests::{fixture_session, login_pending as fixture_pending_login};
