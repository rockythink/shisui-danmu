use super::{AccountClient, AccountStatus, LoginPoll, UiEvent, qr_lines};
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
    mode: Mode,
    account: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remembered_automatic: Option<AutomaticGrant>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AutomaticGrant {
    room_id: String,
    user_id: String,
}

#[derive(Debug)]
pub(super) enum AccountEvent {
    Qr { token: Uuid, lines: Vec<String> },
    Scanned { token: Uuid },
    Ready { token: Uuid, status: AccountStatus },
    Failed { token: Uuid, message: String },
    Unavailable { generation: u64 },
}

struct PendingLogin {
    token: Uuid,
    account: AccountClient,
    status: Option<AccountStatus>,
    task: Option<JoinHandle<()>>,
}

struct IdentityGate {
    generation: u64,
    unavailable: bool,
}

pub(super) struct AssistantAccounts {
    main: AccountClient,
    independent: Option<AccountClient>,
    independent_status: AccountStatus,
    selection: Selection,
    selection_path: PathBuf,
    credential_dir: PathBuf,
    gate: Arc<Mutex<IdentityGate>>,
    load_error: Option<String>,
    pending: Option<PendingLogin>,
    qr: Option<Vec<String>>,
}

impl AssistantAccounts {
    pub(super) fn load(main: &AccountClient, config_path: &Path) -> Result<Self> {
        let selection_path = config_path.with_file_name("assistant-account.json");
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
            .map(|id| AccountClient::new(credential_dir.join(format!("{id}.json"))))
            .transpose()?;
        // A missing or damaged independent credential must never select the main account.
        let independent_status = independent
            .as_ref()
            .and_then(|account| account.cached_status().ok())
            .unwrap_or(AccountStatus::SignedOut);
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

    pub(super) fn generation(&self) -> u64 {
        self.gate.lock().expect("助手身份锁").generation
    }

    pub(super) fn label(&self, main: &AccountStatus) -> String {
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

    pub(super) fn status<'a>(&'a self, main: &'a AccountStatus) -> &'a AccountStatus {
        if self.selection.mode == Mode::ReuseMain {
            main
        } else {
            &self.independent_status
        }
    }

    pub(super) fn independent_user_id<'a>(&'a self, main: &AccountStatus) -> Option<&'a str> {
        if self.selection.mode != Mode::Independent || same_user(&self.independent_status, main) {
            return None;
        }
        match &self.independent_status {
            AccountStatus::SignedIn { user_id, .. } if !user_id.is_empty() => Some(user_id),
            _ => None,
        }
    }

    pub(super) fn pending_status(&self) -> Option<&AccountStatus> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.status.as_ref())
    }

    #[cfg(test)]
    pub(super) fn pending_is_main(&self, main: &AccountStatus) -> bool {
        self.pending_status()
            .is_some_and(|pending| same_user(pending, main))
    }

    pub(super) fn qr_lines(&self) -> Option<&[String]> {
        self.qr.as_deref()
    }
    pub(super) fn login_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Caller pauses the runner and revokes all bridge permits, including one-shot approval.
    pub(super) fn main_changed(&mut self) -> Result<()> {
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

    pub(super) fn start_login(&mut self, tx: mpsc::Sender<UiEvent>) {
        self.cancel_login();
        self.gate.lock().expect("助手身份锁").generation += 1;
        let token = Uuid::new_v4();
        let account = self.main.staged();
        let worker = account.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = login(token, worker, &tx).await {
                let _ = tx
                    .send(UiEvent::AssistantAccount(AccountEvent::Failed {
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

    pub(super) fn cancel_login(&mut self) {
        if let Some(mut pending) = self.pending.take()
            && let Some(task) = pending.task.take()
        {
            task.abort();
        }
        self.qr = None;
    }

    pub(super) fn apply_event(&mut self, event: AccountEvent) -> Option<String> {
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
            AccountEvent::Qr { lines, .. } => {
                self.qr = Some(lines);
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

    pub(super) fn validate_independent(&self, main: &AccountStatus) -> Result<()> {
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
    pub(super) fn complete_independent_ready(&mut self, main: &AccountStatus) -> Result<String> {
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

    pub(super) fn confirm_independent(&mut self, main: &AccountStatus) -> Result<()> {
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

    pub(super) fn reuse_main(&mut self) -> Result<()> {
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

    pub(super) fn sign_out_independent(&mut self) -> Result<()> {
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

    pub(super) fn validate_automatic_identity(
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

    pub(super) fn remember_automatic(&mut self, room_id: &str, main: &AccountStatus) -> Result<()> {
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

    pub(super) fn forget_automatic(&mut self) -> Result<()> {
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

    pub(super) fn automatic_matches(&self, room_id: &str, main: &AccountStatus) -> bool {
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

    pub(super) fn has_automatic_grant(&self) -> bool {
        self.selection.remembered_automatic.is_some()
    }

    pub(super) fn send_identity(&self, main: &AccountStatus) -> SendIdentity {
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

async fn login(token: Uuid, account: AccountClient, tx: &mpsc::Sender<UiEvent>) -> Result<()> {
    let challenge =
        tokio::time::timeout(Duration::from_secs(10), account.login_challenge()).await??;
    let lines = qr_lines(challenge.url.as_str())?;
    tx.send(UiEvent::AssistantAccount(AccountEvent::Qr { token, lines }))
        .await?;
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
                tx.send(UiEvent::AssistantAccount(AccountEvent::Scanned { token }))
                    .await?;
            }
            LoginPoll::Expired => anyhow::bail!("二维码已过期，请重新扫码"),
            LoginPoll::SignedIn(status) => {
                tx.send(UiEvent::AssistantAccount(AccountEvent::Ready {
                    token,
                    status,
                }))
                .await?;
                return Ok(());
            }
        }
    }
}

/// An immutable credential snapshot plus a revocable assistant identity generation.
/// Manual sends never share the assistant's unavailable latch.
pub(super) struct SendIdentity {
    account: Option<AccountClient>,
    generation: u64,
    gate: Option<Arc<Mutex<IdentityGate>>>,
    independent_main: Option<AccountClient>,
    expected_status: Option<AccountStatus>,
}

impl SendIdentity {
    pub(super) fn manual(account: AccountClient) -> Self {
        Self {
            account: account.snapshot().ok(),
            generation: 0,
            gate: None,
            independent_main: None,
            expected_status: None,
        }
    }
    pub(super) fn is_independent(&self) -> bool {
        self.independent_main.is_some()
    }

    pub(super) fn valid(&self) -> bool {
        self.gate.as_ref().is_none_or(|gate| {
            let gate = gate.lock().expect("助手身份锁");
            gate.generation == self.generation && !gate.unavailable
        })
    }

    pub(super) fn account(&self) -> Option<&AccountClient> {
        self.account.as_ref()
    }

    pub(super) async fn status(&self) -> Result<AccountStatus> {
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

    pub(super) fn invalidate(&self) -> Option<u64> {
        let mut gate = self.gate.as_ref()?.lock().expect("助手身份锁");
        if gate.generation != self.generation {
            return None;
        }
        gate.unavailable = true;
        Some(self.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(root: &Path, file: &str, uid: &str) -> AccountClient {
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

    fn stage(accounts: &mut AssistantAccounts, fixture: &AccountClient) -> Uuid {
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
    fn account_isolation_confirmation_cancel_and_restart() {
        let temp = tempfile::tempdir().unwrap();
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
                    lines: vec!["late".into()]
                })
                .is_none()
        );
        assert!(!accounts.login_pending());
        assert!(accounts.qr_lines().is_none());
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
    fn account_isolation_same_uid_and_failed_commit_preserve_old_credentials() {
        let temp = tempfile::tempdir().unwrap();
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
    fn account_isolation_expired_and_signed_out_never_fall_back() {
        let temp = tempfile::tempdir().unwrap();
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
    fn account_isolation_corrupt_selection_does_not_disable_manual() {
        let temp = tempfile::tempdir().unwrap();
        let main = account(temp.path(), "main.json", "11");
        let status = main.cached_status().unwrap();
        let broken = b"{broken";
        std::fs::write(temp.path().join("assistant-account.json"), broken).unwrap();
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
    fn automatic_grant_is_scoped_persisted_and_revocable() {
        let temp = tempfile::tempdir().unwrap();
        let main = account(temp.path(), "main.json", "11");
        let status = main.cached_status().unwrap();
        let config = temp.path().join("config.toml");

        let old_selection = serde_json::json!({ "mode": "reuse_main", "account": null });
        std::fs::write(
            temp.path().join("assistant-account.json"),
            serde_json::to_vec(&old_selection).unwrap(),
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
