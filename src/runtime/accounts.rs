use super::{AccountClient, AccountStatus, CoreEvent, LoginPoll};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};
use uuid::Uuid;

#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    ReuseMain,
    Independent,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    pub(crate) mode: Mode,
    pub(crate) account: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remembered_automatic: Option<AutomaticGrant>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AutomaticGrant {
    pub(crate) room_id: String,
    pub(crate) user_id: String,
}

#[derive(Debug)]
pub(crate) enum AccountEvent {
    Qr { token: Uuid, url: String },
    Scanned { token: Uuid },
    Ready { token: Uuid, status: AccountStatus },
    Failed { token: Uuid, message: String },
    Unavailable { generation: u64 },
}

struct PendingLogin {
    pub(crate) token: Uuid,
    pub(crate) account: AccountClient,
    pub(crate) status: Option<AccountStatus>,
    pub(crate) task: Option<JoinHandle<()>>,
}

struct IdentityGate {
    pub(crate) generation: u64,
    pub(crate) unavailable: bool,
}

pub(crate) struct AssistantAccounts {
    pub(crate) main: AccountClient,
    pub(crate) independent: Option<AccountClient>,
    pub(crate) independent_status: AccountStatus,
    selection: Selection,
    pub(crate) selection_path: PathBuf,
    pub(crate) credential_dir: PathBuf,
    gate: Arc<Mutex<IdentityGate>>,
    pub(crate) load_error: Option<String>,
    pending: Option<PendingLogin>,
    pub(crate) qr: Option<String>,
}

impl AssistantAccounts {
    pub(crate) fn load(main: &AccountClient, config_path: &Path) -> Result<Self> {
        let selection_path = config_path.with_file_name("assistant-account.json");
        crate::persistence::check_path(&selection_path)?;
        crate::workspace_config::check_file(&selection_path)?;
        if let Ok(metadata) = std::fs::metadata(&selection_path) {
            ensure!(
                metadata.len() <= 256 * 1024,
                "助手账号选择文件超过读取上限；原件保留"
            );
        }
        let loaded: Result<Selection> = match std::fs::read(&selection_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("助手账号选择文件损坏"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Selection::default()),
            Err(error) => Err(error).context("助手账号选择文件不可读"),
        };
        let (selection, load_error) = match loaded {
            Ok(selection) => (selection, None),
            Err(error) => (
                Selection {
                    mode: Mode::Independent,
                    account: None,
                    remembered_automatic: None,
                },
                Some(format!("{error}；助手不可用，请重扫或明确复用主账号")),
            ),
        };
        let credential_dir = main
            .session_path()
            .and_then(Path::parent)
            .context("主账号没有私有凭据目录")?
            .join("AssistantAccounts");
        let independent = selection
            .account
            .map(|id| {
                let path = credential_dir.join(format!("{id}.json"));
                crate::persistence::check_path(&path)?;
                crate::workspace_config::check_file(&path)?;
                if let Ok(metadata) = std::fs::metadata(&path) {
                    ensure!(
                        metadata.len() <= 256 * 1024,
                        "独立账号凭据超过读取上限；原件保留"
                    );
                }
                AccountClient::new(path)
            })
            .transpose()?;
        // A missing or damaged independent credential must never select the main account.
        let independent_status = match &independent {
            Some(account) => account.cached_status()?,
            None => AccountStatus::SignedOut,
        };
        Ok(Self {
            main: main.clone(),
            independent,
            independent_status,
            selection,
            selection_path,
            credential_dir,
            gate: Arc::new(Mutex::new(IdentityGate {
                generation: 1,
                unavailable: load_error.is_some(),
            })),
            pending: None,
            qr: None,
            load_error,
        })
    }
    pub(crate) fn check_loaded(&self) -> Result<()> {
        match &self.load_error {
            Some(error) => Err(anyhow::anyhow!(error.clone())),
            None => Ok(()),
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.gate.lock().expect("助手身份锁").generation
    }

    pub(crate) fn label(&self, main: &AccountStatus) -> String {
        if let Some(error) = &self.load_error {
            return error.clone();
        }
        let (prefix, status) = if self.selection.mode == Mode::ReuseMain {
            ("复用主账号", main)
        } else {
            ("独立助手账号", &self.independent_status)
        };
        let identity = match status {
            AccountStatus::SignedIn {
                display_name,
                user_id,
            } => format!("{display_name} · UID {user_id}"),
            AccountStatus::SignedOut => "未登录".into(),
        };
        let warning = if self.gate.lock().expect("助手身份锁").unavailable {
            " · 登录态不可用，助手已停发"
        } else if self.selection.mode == Mode::Independent && same_user(status, main) {
            " · 与主账号相同，不是独立身份；请重扫或复用"
        } else {
            ""
        };
        format!("{prefix} · {identity}{warning}")
    }

    pub(crate) fn status<'a>(&'a self, main: &'a AccountStatus) -> &'a AccountStatus {
        if self.selection.mode == Mode::ReuseMain {
            main
        } else {
            &self.independent_status
        }
    }

    pub(crate) fn independent_user_id<'a>(&'a self, main: &AccountStatus) -> Option<&'a str> {
        if self.selection.mode != Mode::Independent || same_user(&self.independent_status, main) {
            return None;
        }
        match &self.independent_status {
            AccountStatus::SignedIn { user_id, .. } if !user_id.is_empty() => Some(user_id),
            _ => None,
        }
    }

    pub(crate) fn pending_status(&self) -> Option<&AccountStatus> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.status.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn pending_is_main(&self, main: &AccountStatus) -> bool {
        self.pending_status()
            .is_some_and(|pending| same_user(pending, main))
    }

    pub(crate) fn qr_url(&self) -> Option<&str> {
        self.qr.as_deref()
    }
    pub(crate) fn login_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(crate) fn pending_token(&self) -> Option<Uuid> {
        self.pending.as_ref().map(|pending| pending.token)
    }

    /// Caller pauses the runner and revokes all bridge permits, including one-shot approval.
    pub(crate) fn main_changed(&mut self) -> Result<()> {
        self.gate.lock().expect("助手身份锁").generation += 1;
        let result = self.forget_automatic();
        let mut gate = self.gate.lock().expect("助手身份锁");
        if result.is_err() {
            gate.unavailable = true;
        } else if self.selection.mode == Mode::ReuseMain {
            gate.unavailable = false;
        }
        result
    }

    pub(crate) fn start_login_with_token(&mut self, tx: mpsc::Sender<CoreEvent>, token: Uuid) {
        self.cancel_login();
        self.gate.lock().expect("助手身份锁").generation += 1;
        let account = self.main.staged();
        let worker = account.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = login(token, worker, &tx).await {
                let _ = tx
                    .send(CoreEvent::AssistantAccount(AccountEvent::Failed {
                        token,
                        message: format!("独立账号登录失败：{error}；原账号未更改"),
                    }))
                    .await;
            }
        });
        self.pending = Some(PendingLogin {
            token,
            account,
            status: None,
            task: Some(task),
        });
    }

    pub(crate) fn cancel_login(&mut self) {
        if let Some(mut pending) = self.pending.take()
            && let Some(task) = pending.task.take()
        {
            task.abort();
        }
        self.qr = None;
    }

    pub(crate) fn apply_event(&mut self, event: AccountEvent) -> Option<String> {
        if let AccountEvent::Unavailable { generation } = event {
            let gate = self.gate.lock().expect("助手身份锁");
            return (generation == gate.generation && gate.unavailable).then(|| {
                "助手登录态不可用，已停止助手发送；人工主账号不受影响，请重新登录并重新确认许可"
                    .into()
            });
        }
        let token = match &event {
            AccountEvent::Qr { token, .. }
            | AccountEvent::Scanned { token }
            | AccountEvent::Ready { token, .. }
            | AccountEvent::Failed { token, .. } => *token,
            AccountEvent::Unavailable { .. } => unreachable!(),
        };
        let pending = self
            .pending
            .as_mut()
            .filter(|pending| pending.token == token)?;
        match event {
            AccountEvent::Qr { url, .. } => {
                self.qr = Some(url);
                Some("请用 AI 发送账号扫码；扫码成功后将直接启用".into())
            }
            AccountEvent::Scanned { .. } => Some("助手二维码已扫码，请在 B 站客户端确认".into()),
            AccountEvent::Ready { status, .. } => {
                pending.status = Some(status);
                self.qr = None;
                Some("扫码成功，正在验证独立账号".into())
            }
            AccountEvent::Failed { message, .. } => {
                self.cancel_login();
                Some(message)
            }
            AccountEvent::Unavailable { .. } => unreachable!(),
        }
    }

    pub(crate) fn validate_independent(&self, main: &AccountStatus) -> Result<()> {
        let pending = self.pending.as_ref().context("请先扫描独立账号二维码")?;
        let status = pending.status.as_ref().context("扫码尚未完成")?;
        ensure!(
            matches!(main, AccountStatus::SignedIn { user_id, .. } if !user_id.is_empty()),
            "请先验证主账号 UID，才能确认是独立身份"
        );
        ensure!(
            !same_user(status, main),
            "该 UID 与主账号相同，不能称为独立账号；请重新扫码或选择复用主账号"
        );
        ensure!(
            !same_user(status, &self.main.cached_status()?),
            "扫码 UID 与当前主凭据相同；请重扫或明确复用"
        );
        ensure!(
            matches!(status, AccountStatus::SignedIn { user_id, .. } if !user_id.is_empty()),
            "暂存登录态无效，请重新扫码"
        );
        Ok(())
    }

    /// Commits the accepted Ready event currently staged by apply_event.
    /// Call only from the Some branch returned for that Ready event: stale or
    /// cancelled tokens never reach this method. Identity is revalidated here.
    pub(crate) fn complete_independent_ready(&mut self, main: &AccountStatus) -> Result<String> {
        self.validate_independent(main)?;
        let status = self.pending_status().cloned().context("扫码尚未完成")?;
        self.confirm_independent(main)?;
        let AccountStatus::SignedIn {
            display_name,
            user_id,
        } = status
        else {
            unreachable!("validate_independent only accepts a signed-in identity")
        };
        Ok(format!(
            "已启用 AI 发送账号：{display_name} · UID {user_id}"
        ))
    }

    pub(crate) fn confirm_independent(&mut self, main: &AccountStatus) -> Result<()> {
        self.validate_independent(main)?;
        let pending = self.pending.as_ref().context("请先扫描独立账号二维码")?;
        let status = pending.status.as_ref().context("扫码尚未完成")?;
        let id = Uuid::new_v4();
        let account = pending
            .account
            .persist_to(self.credential_dir.join(format!("{id}.json")))?;
        let selection = Selection {
            mode: Mode::Independent,
            account: Some(id),
            remembered_automatic: None,
        };
        if let Err(error) = self.save_selection(&selection) {
            let _ = account.sign_out();
            return Err(error);
        }
        self.gate.lock().expect("助手身份锁").generation += 1;
        let old = self.independent.replace(account);
        self.independent_status = status.clone();
        self.selection = selection;
        self.cancel_login();
        self.gate.lock().expect("助手身份锁").unavailable = false;
        self.load_error = None;
        if let Some(old) = old {
            old.sign_out()
                .context("新账号已启用，但清除旧独立凭据失败")?;
        }
        Ok(())
    }

    pub(crate) fn reuse_main(&mut self) -> Result<()> {
        self.cancel_login();
        self.gate.lock().expect("助手身份锁").generation += 1;
        let selection = Selection {
            mode: Mode::ReuseMain,
            account: self.selection.account,
            remembered_automatic: None,
        };
        self.save_selection(&selection)?;
        self.selection = selection;
        self.gate.lock().expect("助手身份锁").unavailable = false;
        self.load_error = None;
        Ok(())
    }
    pub(crate) fn use_independent(&mut self, main: &AccountStatus) -> Result<()> {
        ensure!(
            self.independent.is_some()
                && matches!(self.independent_status, AccountStatus::SignedIn { .. }),
            "请先扫码登录独立助手账号"
        );
        ensure!(
            !same_user(&self.independent_status, main),
            "助手UID与主账号相同；请重扫或明确复用"
        );
        self.cancel_login();
        self.gate.lock().expect("助手身份锁").generation += 1;
        let selection = Selection {
            mode: Mode::Independent,
            account: self.selection.account,
            remembered_automatic: None,
        };
        self.save_selection(&selection)?;
        self.selection = selection;
        self.gate.lock().expect("助手身份锁").unavailable = false;
        Ok(())
    }

    pub(crate) fn sign_out_independent(&mut self) -> Result<()> {
        self.cancel_login();
        self.gate.lock().expect("助手身份锁").generation += 1;
        // Keep independent mode when selected: logging out is never consent to use the main identity.
        let selection = Selection {
            mode: self.selection.mode,
            account: None,
            remembered_automatic: None,
        };
        self.save_selection(&selection)?;
        self.selection = selection;
        self.independent_status = AccountStatus::SignedOut;
        if let Some(account) = self.independent.take() {
            account.sign_out()?;
        }
        Ok(())
    }

    fn save_selection(&self, selection: &Selection) -> Result<()> {
        crate::storage::write_private_atomic(&self.selection_path, &serde_json::to_vec(selection)?)
            .context("保存助手账号选择失败；原选择保留")
    }

    pub(crate) fn validate_automatic_identity(
        &self,
        room_id: &str,
        main: &AccountStatus,
    ) -> Result<()> {
        ensure!(!room_id.is_empty(), "直播间 ID 为空，不能记住自动发送许可");
        let identity = self.send_identity(main);
        ensure!(identity.valid(), "助手身份已更改或登录态不可用");
        let snapshot = identity.account().context("当前发送身份没有可用凭据")?;
        let cached = snapshot.cached_status()?;
        let selected = self.status(main);
        ensure!(
            matches!(selected, AccountStatus::SignedIn { user_id, .. } if !user_id.is_empty()),
            "当前发送身份未登录，不能记住自动发送许可"
        );
        ensure!(
            same_user(selected, &cached),
            "发送凭据 UID 与已加载身份不一致，不能记住自动发送许可"
        );
        if self.selection.mode == Mode::Independent {
            ensure!(
                !same_user(selected, main),
                "助手 UID 与主账号相同，不能记住自动发送许可"
            );
        }
        Ok(())
    }

    pub(crate) fn remember_automatic(&mut self, room_id: &str, main: &AccountStatus) -> Result<()> {
        ensure!(!room_id.is_empty(), "直播间 ID 为空，不能记住自动发送许可");
        let identity = self.send_identity(main);
        ensure!(identity.valid(), "助手身份已更改或登录态不可用");
        let snapshot = identity.account().context("当前发送身份没有可用凭据")?;
        let cached = snapshot.cached_status()?;
        let selected = self.status(main);
        let user_id = match selected {
            AccountStatus::SignedIn { user_id, .. } if !user_id.is_empty() => user_id,
            _ => anyhow::bail!("当前发送身份未登录，不能记住自动发送许可"),
        };
        ensure!(
            same_user(selected, &cached),
            "发送凭据 UID 与已加载身份不一致，不能记住自动发送许可"
        );
        if self.selection.mode == Mode::Independent {
            ensure!(
                !same_user(selected, main),
                "助手 UID 与主账号相同，不能记住自动发送许可"
            );
        }
        let selection = Selection {
            mode: self.selection.mode,
            account: self.selection.account,
            remembered_automatic: Some(AutomaticGrant {
                room_id: room_id.to_owned(),
                user_id: user_id.to_owned(),
            }),
        };
        self.save_selection(&selection)?;
        self.selection = selection;
        Ok(())
    }

    pub(crate) fn forget_automatic(&mut self) -> Result<()> {
        if self.selection.remembered_automatic.is_none() {
            return Ok(());
        }
        let selection = Selection {
            mode: self.selection.mode,
            account: self.selection.account,
            remembered_automatic: None,
        };
        self.save_selection(&selection)?;
        self.selection = selection;
        Ok(())
    }

    pub(crate) fn automatic_matches(&self, room_id: &str, main: &AccountStatus) -> bool {
        let Some(grant) = &self.selection.remembered_automatic else {
            return false;
        };
        let gate = self.gate.lock().expect("助手身份锁");
        if gate.unavailable {
            return false;
        }
        drop(gate);
        let credentials_loaded = if self.selection.mode == Mode::ReuseMain {
            true
        } else {
            self.independent.is_some() && !same_user(&self.independent_status, main)
        };
        credentials_loaded
            && grant.room_id == room_id
            && matches!(self.status(main), AccountStatus::SignedIn { user_id, .. }
                if !user_id.is_empty() && user_id == &grant.user_id)
    }

    #[cfg(test)]
    pub(crate) fn has_automatic_grant(&self) -> bool {
        self.selection.remembered_automatic.is_some()
    }

    pub(crate) fn send_identity(&self, main: &AccountStatus) -> SendIdentity {
        let account = if self.selection.mode == Mode::ReuseMain {
            Some(&self.main)
        } else {
            self.independent.as_ref()
        };
        SendIdentity {
            account: account.and_then(|account| account.snapshot().ok()),
            generation: self.generation(),
            gate: Some(self.gate.clone()),
            independent_main: (self.selection.mode == Mode::Independent).then(|| self.main.clone()),
            expected_status: Some(self.status(main).clone()),
        }
    }
}

impl Drop for AssistantAccounts {
    fn drop(&mut self) {
        self.cancel_login();
    }
}

fn same_user(left: &AccountStatus, right: &AccountStatus) -> bool {
    matches!((left, right), (AccountStatus::SignedIn { user_id: left, .. }, AccountStatus::SignedIn { user_id: right, .. }) if left == right)
}

async fn login(token: Uuid, account: AccountClient, tx: &mpsc::Sender<CoreEvent>) -> Result<()> {
    let challenge =
        tokio::time::timeout(Duration::from_secs(10), account.login_challenge()).await??;
    let url = challenge.url.to_string();
    tx.send(CoreEvent::AssistantAccount(AccountEvent::Qr { token, url }))
        .await
        .map_err(|_| anyhow::anyhow!("核心已关闭，登录操作未完成"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "二维码已过期，请重新扫码"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        match tokio::time::timeout(Duration::from_secs(10), account.poll_login(&challenge.key))
            .await??
        {
            LoginPoll::Waiting => {}
            LoginPoll::Scanned => {
                tx.send(CoreEvent::AssistantAccount(AccountEvent::Scanned { token }))
                    .await
                    .map_err(|_| anyhow::anyhow!("核心已关闭，登录操作未完成"))?;
            }
            LoginPoll::Expired => anyhow::bail!("二维码已过期，请重新扫码"),
            LoginPoll::SignedIn(status) => {
                tx.send(CoreEvent::AssistantAccount(AccountEvent::Ready {
                    token,
                    status,
                }))
                .await
                .map_err(|_| anyhow::anyhow!("核心已关闭，登录操作未完成"))?;
                return Ok(());
            }
        }
    }
}

/// An immutable credential snapshot plus a revocable assistant identity generation.
/// Manual sends never share the assistant's unavailable latch.
pub(crate) struct SendIdentity {
    pub(crate) account: Option<AccountClient>,
    pub(crate) generation: u64,
    gate: Option<Arc<Mutex<IdentityGate>>>,
    pub(crate) independent_main: Option<AccountClient>,
    pub(crate) expected_status: Option<AccountStatus>,
}

impl SendIdentity {
    pub(crate) fn manual(account: AccountClient) -> Self {
        Self {
            account: account.snapshot().ok(),
            generation: 0,
            gate: None,
            independent_main: None,
            expected_status: None,
        }
    }
    pub(crate) fn is_independent(&self) -> bool {
        self.independent_main.is_some()
    }

    pub(crate) fn valid(&self) -> bool {
        self.gate.as_ref().is_none_or(|gate| {
            let gate = gate.lock().expect("助手身份锁");
            gate.generation == self.generation && !gate.unavailable
        })
    }

    pub(crate) fn account(&self) -> Option<&AccountClient> {
        self.account.as_ref()
    }

    pub(crate) async fn status(&self) -> Result<AccountStatus> {
        ensure!(self.valid(), "助手身份已更改或登录态不可用");
        let account = self.account.as_ref().context("当前发送身份没有可用凭据")?;
        let status = account.status().await?;
        if let Some(expected) = &self.expected_status {
            ensure!(
                same_user(expected, &status),
                "实际发送 UID 与已显示身份不一致；请重新确认账号"
            );
        }
        if let Some(main) = &self.independent_main {
            ensure!(
                !same_user(&status, &main.status().await?),
                "助手 UID 与主账号相同；请重扫或明确选择复用"
            );
        }
        Ok(status)
    }

    pub(crate) fn invalidate(&self) -> Option<u64> {
        let mut gate = self.gate.as_ref()?.lock().expect("助手身份锁");
        if gate.generation != self.generation {
            return None;
        }
        gate.unavailable = true;
        Some(self.generation)
    }
}

use super::{
    DanmuEvent, DanmuSessionEndReason, Local, OperationState, OperationValue, RoomSnapshot,
    RuntimeChange, RuntimeMode, RuntimeState, Utc,
};

impl RuntimeState {
    pub(crate) fn disconnect(&mut self) -> Result<()> {
        self.disconnect_with_policy(true)
    }

    fn disconnect_with_policy(&mut self, forget_automatic: bool) -> Result<()> {
        self.connection_generation = self.connection_generation.wrapping_add(1);
        if let Some(stop) = self.connection_stop.take() {
            let _ = stop.send(true);
        }
        self.connection_tasks.abort_all();
        if forget_automatic {
            self.pause_assistant();
        } else {
            self.cancel_chatgpt_operation();
            self.runner.pause(&self.bridge);
        }
        self.manual_send_queue.pause();
        self.independent_send_queue.pause();
        self.bridge.end();
        self.connection = "未连接".into();
        self.room = None;
        self.watched = None;
        self.likes = None;
        self.online_viewers = None;
        self.resume_pending = false;
        self.restore_scope = None;
        self.connect_operation = None;
        if self.session.room_id.is_empty() {
            Ok(())
        } else {
            self.journal.snapshot(&self.session)
        }
    }

    pub(crate) fn begin_connect(&mut self, room_id: String, operation_id: Uuid) -> Result<()> {
        ensure!(self.mode != RuntimeMode::Replay, "回放模式禁止网络连接");
        ensure!(
            self.mode == RuntimeMode::Local || room_id.parse::<u64>().is_ok_and(|id| id > 0),
            "缺少有效房间号"
        );
        if self.session.room_id == room_id
            && (self.connection_stop.is_some() || self.connection.starts_with("已连接"))
        {
            return Err(anyhow::anyhow!("当前房间已经连接或正在连接"));
        }
        self.disconnect_with_policy(false)?;
        if self.session.room_id != room_id {
            if !self.session.room_id.is_empty() && self.session.ended_at.is_none() {
                self.session
                    .end(Utc::now(), DanmuSessionEndReason::Completed);
                self.journal.end(&self.session)?;
            }
            let session = Self::recover_session(&self.journal, &room_id)?;
            self.journal.start(&session)?;
            self.session = session;
            self.message_keys.clear();
            self.new_message_count = 0;
            self.events_dirty = true;
        }
        self.manual_send_queue.resume();
        self.independent_send_queue.resume();
        self.bridge.new_session();
        self.live_scope = None;
        self.connection = format!("连接中 {room_id}");
        self.connect_operation = Some(operation_id);
        self.runner
            .begin_history_import(&self.session.room_id, &self.journal)?;
        if self.mode == RuntimeMode::Local {
            self.connection = format!("已连接 {room_id}");
            self.ingestion_live = true;
            self.bridge.available(true);
            self.pending_changes.push(RuntimeChange::Operation {
                operation_id,
                state: OperationState::Completed,
                value: OperationValue::None,
            });
            return Ok(());
        }
        let generation = self.connection_generation;
        let client = self.client.clone();
        let account = self.account.clone();
        let tx = self.ui_tx.as_ref().expect("core event channel").clone();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        self.connection_stop = Some(stop);
        self.connection_tasks.spawn(async move {
            let result = tokio::select! {
                _ = stopped.changed() => return,
                result = async {
                    let (room, status, history) = tokio::join!(client.room_snapshot(&room_id), account.status(), client.history(&room_id));
                    Ok((room?, status?, history?))
                } => result,
            };
            let _ = tx.send(CoreEvent::Connected { generation, result }).await;
        });
        Ok(())
    }

    pub(crate) fn finish_connect(
        &mut self,
        generation: u64,
        result: Result<(RoomSnapshot, AccountStatus, Vec<DanmuEvent>)>,
    ) {
        if generation != self.connection_generation {
            return;
        }
        let Some(operation_id) = self.connect_operation.take() else {
            return;
        };
        let (room, status, history) = match result {
            Ok(value) => value,
            Err(error) => {
                self.connection = format!("连接失败：{error}");
                if let Some(stop) = self.connection_stop.take() {
                    let _ = stop.send(true);
                }
                self.finish_operation(operation_id, Err(error));
                return;
            }
        };
        self.account_status = status;
        self.room = Some(room.clone());
        self.room_updated_at = Some(Local::now());
        self.sync_assistant_room();
        self.ingestion_live = false;
        for event in history {
            self.ingest_event(event);
        }
        self.ingestion_live = true;
        let tx = self.ui_tx.as_ref().expect("core event channel").clone();
        let client = self.client.clone();
        let room_id = self.session.room_id.clone();
        let Some(stop) = self.connection_stop.as_ref() else {
            return;
        };
        let stream_stop = stop.subscribe();
        let (client_tx, mut client_rx) = tokio::sync::mpsc::channel(512);
        self.connection_tasks.spawn(async move {
            client.run(room_id, client_tx, stream_stop).await;
        });
        let client_tx = tx.clone();
        self.connection_tasks.spawn(async move {
            while let Some(event) = client_rx.recv().await {
                if client_tx
                    .send(CoreEvent::Client { generation, event })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let client = self.client.clone();
        let account = self.account.clone();
        let mut stopped = stop.subscribe();
        self.connection_tasks.spawn(async move {
            let mut last_room = room;
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    _ = interval.tick() => {
                        match client.room_snapshot_preserving(&last_room.room_id, Some(&last_room)).await {
                            Ok(room) => {
                                last_room = room.clone();
                                if tx.send(CoreEvent::Scoped { generation, event: Box::new(CoreEvent::RoomSnapshot(room)) }).await.is_err() { break; }
                                if !super::send_room_metrics(&account, &tx, &last_room, generation).await { break; }
                            }
                            Err(error) => { let _ = tx.send(CoreEvent::Scoped { generation, event: Box::new(CoreEvent::error(format!("房间数据刷新失败：{error}"))) }).await; }
                        }
                    }
                }
            }
        });
        if self.runner.settings.automatic
            && self.runner.settings.resume_on_start
            && self
                .assistant_accounts
                .automatic_matches(&self.session.room_id, &self.account_status)
        {
            let scope = (
                self.bridge.session_id(),
                self.assistant_accounts.generation(),
            );
            self.resume_pending = true;
            self.restore_scope = Some(scope.clone());
            self.restore_verified = false;
            let identity = self.assistant_accounts.send_identity(&self.account_status);
            let tx = self.ui_tx.as_ref().expect("core event channel").clone();
            self.tasks.spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(10), identity.status())
                    .await
                    .map_err(|_| anyhow::anyhow!("发送身份验证超时"))
                    .and_then(|result| result.map(|_| ()));
                let _ = tx.send(CoreEvent::ResumeChecked { scope, result }).await;
            });
        }
        self.finish_operation(operation_id, Ok(OperationValue::None));
    }
}

impl RuntimeState {
    pub(crate) fn account_command(&mut self, command: super::AccountCommand) -> Result<bool> {
        use super::{AccountCommand, Identity};
        ensure!(self.accounts_allowed(), "本地与回放模式禁止真实账号操作");
        match command {
            AccountCommand::BeginLogin { identity } => {
                self.pause_assistant();
                self.bridge.identity_changed();
                let tx = self.ui_tx.as_ref().context("核心事件通道尚未准备")?.clone();
                match identity {
                    Identity::Main => self.start_login(tx),
                    Identity::Assistant => {
                        self.cancel_main_login();
                        let token = self.operation_token();
                        self.assistant_accounts.start_login_with_token(tx, token);
                    }
                }
                return Ok(true);
            }
            AccountCommand::CancelLogin => {
                self.cancel_main_login();
                if let Some(token) = self.assistant_accounts.pending_token() {
                    self.assistant_accounts.cancel_login();
                    self.operations.insert(token, OperationState::Cancelled);
                    self.pending_changes.push(RuntimeChange::Operation {
                        operation_id: token,
                        state: OperationState::Cancelled,
                        value: OperationValue::None,
                    });
                }
                self.refresh_login_projection();
            }
            AccountCommand::SignOut { identity } => {
                self.pause_assistant();
                self.bridge.identity_changed();
                match identity {
                    Identity::Main => {
                        self.cancel_main_login();
                        self.manual_send_queue.pause();
                        self.manual_send_queue.resume();
                        let forgotten = self.assistant_accounts.main_changed();
                        self.account.sign_out()?;
                        self.account_status = AccountStatus::SignedOut;
                        forgotten?;
                    }
                    Identity::Assistant => self.assistant_accounts.sign_out_independent()?,
                }
            }
            AccountCommand::UseMainForAssistant => {
                self.pause_assistant();
                self.bridge.identity_changed();
                self.assistant_accounts.reuse_main()?;
            }
            AccountCommand::UseIndependentAssistant => {
                self.pause_assistant();
                self.bridge.identity_changed();
                self.assistant_accounts
                    .use_independent(&self.account_status)?;
            }
        }
        #[cfg(feature = "tui")]
        {
            self.review_frame = None;
        }
        Ok(false)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn account(root: &Path, file: &str, uid: &str) -> AccountClient {
        let path = root.join(file);
        let status = AccountStatus::SignedIn {
            display_name: format!("用户{uid}"),
            user_id: uid.into(),
        };
        crate::storage::write_private_atomic(&path, &serde_json::to_vec(&serde_json::json!({
            "cookieHeader": format!("SESSDATA=secret-{uid}; bili_jct=csrf-{uid}; DedeUserID={uid}"),
            "csrf": format!("csrf-{uid}"), "identity": status
        })).unwrap()).unwrap();
        AccountClient::new(path).unwrap()
    }

    pub(crate) fn stage(accounts: &mut AssistantAccounts, fixture: &AccountClient) -> Uuid {
        let token = Uuid::new_v4();
        accounts.pending = Some(PendingLogin {
            token,
            account: fixture.snapshot().unwrap(),
            status: Some(fixture.cached_status().unwrap()),
            task: None,
        });
        token
    }

    #[test]
    pub(crate) fn account_isolation_confirmation_cancel_and_restart() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let main = account(temp.path(), "main.json", "11");
        let status = main.cached_status().unwrap();
        let config = temp.path().join("config.toml");
        let mut accounts = AssistantAccounts::load(&main, &config).unwrap();
        let independent = account(temp.path(), "fixture.json", "22");
        let accepted = stage(&mut accounts, &independent);
        assert!(
            accounts
                .apply_event(AccountEvent::Ready {
                    token: accepted,
                    status: independent.cached_status().unwrap(),
                })
                .is_some(),
            "current Ready token must be accepted"
        );
        let message = accounts.complete_independent_ready(&status).unwrap();
        assert!(message.contains("用户22") && message.contains("UID 22"));
        let old = accounts.send_identity(&status);
        let replacement = account(temp.path(), "fixture-new.json", "33");
        let cancelled = stage(&mut accounts, &replacement);
        accounts.cancel_login();
        assert!(
            accounts
                .apply_event(AccountEvent::Ready {
                    token: cancelled,
                    status: replacement.cached_status().unwrap()
                })
                .is_none()
        );
        assert!(
            accounts
                .apply_event(AccountEvent::Qr {
                    token: cancelled,
                    url: "https://example.test/late".into()
                })
                .is_none()
        );
        assert!(!accounts.login_pending());
        assert!(accounts.qr_url().is_none());
        let restored = AssistantAccounts::load(&main, &config).unwrap();
        assert!(same_user(
            restored.status(&status),
            &independent.cached_status().unwrap()
        ));
        accounts.reuse_main().unwrap();
        assert!(!old.valid());
        assert!(same_user(
            &old.account().unwrap().cached_status().unwrap(),
            &independent.cached_status().unwrap()
        ));
        assert!(same_user(&main.cached_status().unwrap(), &status));
        let selection_text = std::fs::read_to_string(&accounts.selection_path).unwrap();
        assert!(
            !selection_text.contains("secret")
                && !selection_text.contains("csrf")
                && !selection_text.contains("cookie")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&accounts.selection_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    pub(crate) fn account_isolation_same_uid_and_failed_commit_preserve_old_credentials() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let main = account(temp.path(), "main.json", "11");
        let status = main.cached_status().unwrap();
        let mut accounts =
            AssistantAccounts::load(&main, &temp.path().join("config.toml")).unwrap();
        let old = account(temp.path(), "old.json", "22");
        stage(&mut accounts, &old);
        accounts.confirm_independent(&status).unwrap();
        let sender = accounts.send_identity(&status);
        assert!(accounts.confirm_independent(&status).is_err());
        assert!(
            sender.valid(),
            "no pending confirmation must retain old identity permits"
        );
        let same_uid = stage(&mut accounts, &main);
        assert!(accounts.pending_is_main(&status));
        assert!(
            accounts
                .apply_event(AccountEvent::Ready {
                    token: same_uid,
                    status: main.cached_status().unwrap(),
                })
                .is_some()
        );
        assert!(accounts.complete_independent_ready(&status).is_err());
        assert!(
            sender.valid(),
            "same-UID rejection must retain old identity permits"
        );
        assert!(same_user(
            accounts.status(&status),
            &old.cached_status().unwrap()
        ));
        let replacement = account(temp.path(), "new.json", "33");
        let replacement_token = stage(&mut accounts, &replacement);
        assert!(
            accounts
                .apply_event(AccountEvent::Ready {
                    token: replacement_token,
                    status: replacement.cached_status().unwrap(),
                })
                .is_some()
        );
        let selection_before = std::fs::read(&accounts.selection_path).unwrap();
        // A file used as a directory deterministically fails, even when the test runs as root.
        accounts.selection_path = temp.path().join("main.json/invalid");
        assert!(accounts.complete_independent_ready(&status).is_err());
        assert!(
            sender.valid(),
            "failed persistence must retain old identity permits"
        );
        assert!(same_user(
            accounts.status(&status),
            &old.cached_status().unwrap()
        ));
        assert_eq!(
            std::fs::read(temp.path().join("assistant-account.json")).unwrap(),
            selection_before
        );
        assert!(same_user(&main.cached_status().unwrap(), &status));
        assert_eq!(
            std::fs::read_dir(&accounts.credential_dir).unwrap().count(),
            1
        );
    }

    #[test]
    pub(crate) fn account_isolation_expired_and_signed_out_never_fall_back() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let main = account(temp.path(), "main.json", "11");
        let status = main.cached_status().unwrap();
        let config = temp.path().join("config.toml");
        let mut accounts = AssistantAccounts::load(&main, &config).unwrap();
        let independent = account(temp.path(), "independent.json", "22");
        stage(&mut accounts, &independent);
        accounts.confirm_independent(&status).unwrap();
        let sender = accounts.send_identity(&status);
        let manual = SendIdentity::manual(main.clone());
        assert_eq!(sender.invalidate(), Some(accounts.generation()));
        assert!(!accounts.send_identity(&status).valid());
        assert!(manual.valid());
        accounts.sign_out_independent().unwrap();
        assert!(accounts.send_identity(&status).account().is_none());
        let restored = AssistantAccounts::load(&main, &config).unwrap();
        assert!(restored.send_identity(&status).account().is_none());
        assert!(same_user(
            &manual.account().unwrap().cached_status().unwrap(),
            &status
        ));
        accounts.reuse_main().unwrap();
        assert!(accounts.send_identity(&status).valid());
        assert_eq!(sender.invalidate(), None);
        assert!(accounts.send_identity(&status).valid());
    }

    #[test]
    pub(crate) fn account_isolation_corrupt_selection_does_not_disable_manual() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let main = account(temp.path(), "main.json", "11");
        let status = main.cached_status().unwrap();
        let broken = b"{broken";
        crate::storage::write_private_atomic(&temp.path().join("assistant-account.json"), broken)
            .unwrap();
        let mut accounts =
            AssistantAccounts::load(&main, &temp.path().join("config.toml")).unwrap();
        assert!(!accounts.send_identity(&status).valid());
        assert!(accounts.send_identity(&status).account().is_none());
        assert!(SendIdentity::manual(main).valid());
        accounts.forget_automatic().unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("assistant-account.json")).unwrap(),
            broken
        );
        accounts.reuse_main().unwrap();
        assert!(accounts.send_identity(&status).valid());
        assert!(same_user(accounts.status(&status), &status));
    }

    #[test]
    pub(crate) fn automatic_grant_is_scoped_persisted_and_revocable() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let main = account(temp.path(), "main.json", "11");
        let status = main.cached_status().unwrap();
        let config = temp.path().join("config.toml");

        let old_selection = serde_json::json!({ "mode": "reuse_main", "account": null });
        crate::storage::write_private_atomic(
            &temp.path().join("assistant-account.json"),
            &serde_json::to_vec(&old_selection).unwrap(),
        )
        .unwrap();
        let mut accounts = AssistantAccounts::load(&main, &config).unwrap();
        assert!(!accounts.has_automatic_grant());
        assert!(!accounts.automatic_matches("100", &status));

        accounts.remember_automatic("100", &status).unwrap();
        assert!(accounts.has_automatic_grant());
        let mut restored = AssistantAccounts::load(&main, &config).unwrap();
        assert!(restored.automatic_matches("100", &status));
        assert!(!restored.automatic_matches("200", &status));
        assert!(!restored.automatic_matches(
            "100",
            &AccountStatus::SignedIn {
                display_name: "另一个账号".into(),
                user_id: "22".into(),
            }
        ));
        assert!(!restored.automatic_matches("100", &AccountStatus::SignedOut));

        restored.forget_automatic().unwrap();
        assert!(!restored.has_automatic_grant());
        let revoked = AssistantAccounts::load(&main, &config).unwrap();
        assert!(!revoked.has_automatic_grant());
        assert!(!revoked.automatic_matches("100", &status));
    }
}
