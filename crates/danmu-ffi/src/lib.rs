// SPDX-License-Identifier: MPL-2.0
//! Public desktop boundary for the one shared DANMU runtime.
mod types;
pub use types::*;

use shisui_danmu::{obs::ObsPasswordStore, runtime, storage::StoragePaths};
use std::{
    path::{Component, PathBuf},
    sync::Arc,
};

uniffi::setup_scaffolding!();

#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!("test-support is forbidden in release builds");

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("{message}")]
    InvalidInput { message: String },
    #[error("{message}")]
    Busy { message: String },
    #[error("{message}")]
    Stale { message: String },
    #[error("{message}")]
    PermissionDenied { message: String },
    #[error("{message}")]
    Storage { message: String },
    #[error("{message}")]
    Transport { message: String },
    #[error("{message}")]
    Protocol { message: String },
    #[error("{message}")]
    Closed { message: String },
}

impl From<uniffi::UnexpectedUniFFICallbackError> for CoreError {
    fn from(_: uniffi::UnexpectedUniFFICallbackError) -> Self {
        // Never expose foreign exception descriptions: they can contain secrets.
        Self::Storage {
            message: "宿主密码存储回调失败".into(),
        }
    }
}

impl CoreError {
    fn from_runtime(error: anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<Self>() {
            return error.clone();
        }
        if let Some(error) = error.downcast_ref::<runtime::error::RuntimeFailure>() {
            use runtime::error::RuntimeFailureKind;
            let message = safe_summary(&error.message);
            return match error.kind {
                RuntimeFailureKind::InvalidInput => Self::InvalidInput { message },
                RuntimeFailureKind::Busy => Self::Busy { message },
                RuntimeFailureKind::Stale => Self::Stale { message },
                RuntimeFailureKind::PermissionDenied => Self::PermissionDenied { message },
                RuntimeFailureKind::Storage => Self::Storage { message },
                RuntimeFailureKind::Transport => Self::Transport { message },
                RuntimeFailureKind::Protocol => Self::Protocol { message },
                RuntimeFailureKind::Closed => Self::Closed { message },
            };
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind;
            return match error.kind() {
                ErrorKind::WouldBlock => Self::Busy {
                    message: "核心资源正由其他操作或实例占用".into(),
                },
                ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::NotConnected
                | ErrorKind::AddrInUse
                | ErrorKind::AddrNotAvailable
                | ErrorKind::BrokenPipe
                | ErrorKind::TimedOut
                | ErrorKind::HostUnreachable
                | ErrorKind::NetworkUnreachable => Self::Transport {
                    message: format!("网络连接失败（{:?}）", error.kind()),
                },
                _ => Self::Storage {
                    message: format!("核心存储访问失败（{:?}）", error.kind()),
                },
            };
        }
        if let Some(error) = error.downcast_ref::<reqwest::Error>() {
            return if error.is_decode() {
                Self::Protocol {
                    message: "网络响应格式无效".into(),
                }
            } else if error.is_builder() {
                Self::InvalidInput {
                    message: "网络请求配置无效".into(),
                }
            } else {
                Self::Transport {
                    message: "网络请求失败；请检查连接与核心诊断".into(),
                }
            };
        }
        if error.downcast_ref::<serde_json::Error>().is_some() {
            return Self::Protocol {
                message: "核心数据格式错误；原始数据未修改".into(),
            };
        }
        if error.downcast_ref::<url::ParseError>().is_some() {
            return Self::InvalidInput {
                message: "地址格式无效".into(),
            };
        }
        // Runtime guards carry safe user-facing summaries. Do not format the
        // anyhow chain, Debug representation, headers, or request bodies here.
        Self::Protocol {
            message: safe_summary(&error.to_string()),
        }
    }
}

fn safe_summary(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(512)
        .collect()
}

fn absolute_path(value: String, label: &str) -> Result<PathBuf, CoreError> {
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(CoreError::InvalidInput {
            message: format!("{label}必须是无上级跳转的绝对路径"),
        });
    }
    // The runtime applies its existing symlink/private-file checks before use.
    Ok(path)
}

fn path_string(value: PathBuf) -> Result<String, CoreError> {
    value
        .into_os_string()
        .into_string()
        .map_err(|_| CoreError::Protocol {
            message: "核心路径不是有效 UTF-8，无法交给桌面宿主".into(),
        })
}

fn parse_uuid(value: &str) -> Result<uuid::Uuid, CoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| CoreError::InvalidInput {
        message: "操作或确认标识格式无效".into(),
    })
}

fn parse_time(value: i64) -> Result<chrono::DateTime<chrono::Utc>, CoreError> {
    chrono::DateTime::from_timestamp_millis(value).ok_or_else(|| CoreError::InvalidInput {
        message: "时间超出支持的 Unix 毫秒范围".into(),
    })
}

/// Implemented by the public Swift Keychain companion, not private GUI code.
/// A store must not retain its DesktopRuntime, to avoid a cross-language cycle.
#[uniffi::export(foreign)]
pub trait DesktopObsPasswordStore: Send + Sync {
    fn read(&self, host: String, port: u16) -> Result<Option<String>, CoreError>;
    fn write(&self, host: String, port: u16, password: String) -> Result<(), CoreError>;
}

struct PasswordStoreAdapter(Arc<dyn DesktopObsPasswordStore>);
impl ObsPasswordStore for PasswordStoreAdapter {
    fn read(&self, host: &str, port: u16) -> anyhow::Result<Option<String>> {
        self.0
            .read(host.to_owned(), port)
            .map_err(anyhow::Error::new)
    }
    fn write(&self, host: &str, port: u16, password: &str) -> anyhow::Result<()> {
        self.0
            .write(host.to_owned(), port, password.to_owned())
            .map_err(anyhow::Error::new)
    }
}

#[derive(uniffi::Object)]
pub struct DesktopRuntime {
    handle: runtime::RuntimeHandle,
    #[cfg(feature = "test-support")]
    qa_local: bool,
}

#[uniffi::export(async_runtime = "tokio")]
impl DesktopRuntime {
    #[uniffi::constructor]
    pub async fn open(
        options: DesktopOptions,
        obs_passwords: Arc<dyn DesktopObsPasswordStore>,
    ) -> Result<Arc<Self>, CoreError> {
        #[cfg(feature = "test-support")]
        let _gate = QA_SETUP_GATE.lock().await;
        #[cfg(feature = "test-support")]
        QA_OPENED.store(true, std::sync::atomic::Ordering::Release);
        #[cfg(feature = "test-support")]
        let qa_local = options.mode == DesktopMode::Local;

        let DesktopOptions {
            support_root,
            config_path,
            account_path,
            obs_config_path,
            sessions_root,
            workspace_default,
            external_instance,
            mode,
        } = options;
        let config_file = absolute_path(config_path, "config_path")?;
        let storage = StoragePaths {
            support_dir: absolute_path(support_root, "support_root")?,
            account_session: absolute_path(account_path, "account_path")?,
            obs_configuration: absolute_path(obs_config_path, "obs_config_path")?,
            sessions_dir: absolute_path(sessions_root, "sessions_root")?,
            themes_file: config_file.with_file_name("themes.json"),
            config_file,
        };
        let handle = runtime::open(runtime::RuntimeOptions {
            storage,
            workspace_default: absolute_path(workspace_default, "workspace_default")?,
            external_instance: external_instance
                .map(|p| absolute_path(p, "external_instance"))
                .transpose()?,
            mode: mode.into(),
            obs_passwords: Arc::new(PasswordStoreAdapter(obs_passwords)),
        })
        .await
        .map_err(CoreError::from_runtime)?;
        Ok(Arc::new(Self {
            handle,
            #[cfg(feature = "test-support")]
            qa_local,
        }))
    }

    pub async fn execute(
        &self,
        command: DesktopCommand,
    ) -> Result<DesktopCommandResult, CoreError> {
        self.handle
            .execute(command.try_into()?)
            .await
            .map(Into::into)
            .map_err(CoreError::from_runtime)
    }

    pub async fn query(&self, query: DesktopQuery) -> Result<DesktopQueryResult, CoreError> {
        self.handle
            .query(query.try_into()?)
            .await
            .map_err(CoreError::from_runtime)?
            .try_into()
    }

    pub async fn observe(&self) -> Result<Arc<DesktopSubscription>, CoreError> {
        let subscription = self
            .handle
            .observe()
            .await
            .map_err(CoreError::from_runtime)?;
        Ok(Arc::new(DesktopSubscription {
            subscription,
            consuming: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.handle
            .shutdown()
            .await
            .map_err(CoreError::from_runtime)
    }
}

#[derive(uniffi::Object)]
pub struct DesktopSubscription {
    subscription: runtime::RuntimeSubscription,
    consuming: std::sync::atomic::AtomicBool,
}

struct ConsumerGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for ConsumerGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl DesktopSubscription {
    pub async fn next_batch(&self) -> Result<Option<DesktopBatch>, CoreError> {
        if self
            .consuming
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(CoreError::Busy {
                message: "每条观察订阅只允许一个消费者".into(),
            });
        }
        let _guard = ConsumerGuard(&self.consuming);
        self.subscription
            .next_batch()
            .await
            .map(|batch| batch.map(Into::into))
            .map_err(CoreError::from_runtime)
    }

    pub fn close(&self) {
        self.subscription.close();
    }
}

// QA APIs do not exist in production metadata or normal public records.
#[cfg(feature = "test-support")]
static QA_SETUP_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[cfg(feature = "test-support")]
static QA_OPENED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopQaEndpoints {
    pub account_origin: Option<String>,
    pub obs_port: Option<u16>,
    pub subscription_oauth_origin: Option<String>,
    pub subscription_api_origin: Option<String>,
}

#[cfg(feature = "test-support")]
#[uniffi::export(async_runtime = "tokio")]
pub async fn desktop_qa_install_endpoints(endpoints: DesktopQaEndpoints) -> Result<(), CoreError> {
    let _gate = QA_SETUP_GATE.lock().await;
    if QA_OPENED.load(std::sync::atomic::Ordering::Acquire) {
        return Err(CoreError::Busy {
            message: "QA端点必须在打开首个核心实例前安装".into(),
        });
    }
    if endpoints.subscription_oauth_origin.is_some() != endpoints.subscription_api_origin.is_some()
    {
        return Err(CoreError::InvalidInput {
            message: "QA订阅OAuth与API端点必须成对提供".into(),
        });
    }
    // Validate the complete bundle before changing any process-global endpoint.
    for origin in [
        endpoints.account_origin.as_deref(),
        endpoints.subscription_oauth_origin.as_deref(),
        endpoints.subscription_api_origin.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let url = url::Url::parse(origin).map_err(|_| CoreError::InvalidInput {
            message: "QA端点地址格式无效".into(),
        })?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.path() != "/"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CoreError::InvalidInput {
                message: "QA端点只允许literal HTTP 127.0.0.1 origin".into(),
            });
        }
    }
    if endpoints.obs_port == Some(0) {
        return Err(CoreError::InvalidInput {
            message: "QA OBS端口必须非零".into(),
        });
    }
    if let Some(origin) = endpoints.account_origin {
        shisui_danmu::test_support::install_fake_account_endpoint(&origin)
            .map_err(CoreError::from_runtime)?;
    }
    if let Some(port) = endpoints.obs_port {
        shisui_danmu::test_support::install_fake_obs_endpoint(port)
            .map_err(CoreError::from_runtime)?;
    }
    if let (Some(oauth), Some(api)) = (
        endpoints.subscription_oauth_origin,
        endpoints.subscription_api_origin,
    ) {
        shisui_danmu::test_support::install_subscription_endpoints(&oauth, &api)
            .map_err(CoreError::from_runtime)?;
    }
    Ok(())
}

#[cfg(feature = "test-support")]
#[uniffi::export(async_runtime = "tokio")]
impl DesktopRuntime {
    /// Fixture JSON is only a QA ingress format, never a production command API.
    pub async fn qa_inject_events_json(
        &self,
        events_json: String,
        realtime: bool,
    ) -> Result<(), CoreError> {
        if !self.qa_local {
            return Err(CoreError::PermissionDenied {
                message: "QA注入仅允许隔离Local实例".into(),
            });
        }
        let events = serde_json::from_str::<Vec<shisui_danmu::domain::DanmuEvent>>(&events_json)
            .map_err(|_| CoreError::InvalidInput {
                message: "QA事件必须是标准DanmuEvent数组".into(),
            })?;
        self.handle
            .inject_events(events, realtime)
            .await
            .map_err(CoreError::from_runtime)
    }
}

#[cfg(feature = "test-support")]
#[uniffi::export]
pub fn desktop_qa_subscription_fixture_bundle(origin: String) -> Result<String, CoreError> {
    let value = shisui_danmu::test_support::subscription_fixture_bundle(&origin)
        .map_err(CoreError::from_runtime)?;
    serde_json::to_string(&value).map_err(|_| CoreError::Protocol {
        message: "QA订阅fixture序列化失败".into(),
    })
}

#[cfg(feature = "test-support")]
#[uniffi::export]
pub fn desktop_qa_subscription_login_fixture(
    nonce: String,
    client_id: String,
    subject: String,
) -> Result<String, CoreError> {
    let value =
        shisui_danmu::test_support::subscription_login_fixture(&nonce, &client_id, &subject);
    serde_json::to_string(&value).map_err(|_| CoreError::Protocol {
        message: "QA登录fixture序列化失败".into(),
    })
}

#[cfg(test)]
mod tests;
