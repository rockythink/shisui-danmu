//! Explicit isolated QA support. Never compiled into a release artifact.

#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!("test-support is forbidden in release builds");

use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[cfg(test)]
pub(crate) fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    let directory = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

static OBS_ENDPOINT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

/// Permit Local mode to use only the explicitly installed synthetic OBS server.
pub fn install_fake_obs_endpoint(port: u16) -> Result<()> {
    ensure!(port != 0, "QA OBS port must be nonzero");
    OBS_ENDPOINT
        .set(port)
        .map_err(|_| anyhow::anyhow!("QA OBS endpoint already installed"))
}

pub fn is_fake_obs_endpoint(host: &str, port: u16) -> bool {
    host == "127.0.0.1" && OBS_ENDPOINT.get() == Some(&port)
}

pub fn install_fake_account_endpoint(origin: &str) -> Result<()> {
    check_loopback_origin(origin)?;
    crate::bilibili::AccountClient::install_qa_endpoint(origin)
}

pub fn is_fake_account_endpoint_installed() -> bool {
    crate::bilibili::AccountClient::qa_endpoint_installed()
}

fn check_loopback_origin(origin: &str) -> Result<()> {
    let url = url::Url::parse(origin)?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.path() == "/"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "QA endpoint must be a literal HTTP 127.0.0.1 origin"
    );
    Ok(())
}

/// Call once before opening a QA runtime. Production has no equivalent override.
pub fn install_subscription_endpoints(oauth_origin: &str, api_origin: &str) -> Result<()> {
    check_loopback_origin(oauth_origin)?;
    check_loopback_origin(api_origin)?;
    crate::runner::subscription::install_test_oauth_origin(oauth_origin)?;
    crate::runner::responses::install_test_api_origin(api_origin)?;
    Ok(())
}

/// Same signed fixtures used by the subscription transport regressions.
pub fn subscription_fixture_bundle(origin: &str) -> Result<Value> {
    check_loopback_origin(origin)?;
    Ok(json!({
        "account_id": crate::runner::subscription::FIXTURE_ACCOUNT,
        "discovery": crate::runner::subscription::fixture_discovery(origin),
        "jwks": crate::runner::subscription::fixture_jwks(),
        "refresh": crate::runner::subscription::fixture_token_response("refresh-2")
    }))
}

pub fn subscription_login_fixture(nonce: &str, client_id: &str, subject: &str) -> Value {
    crate::runner::subscription::fixture_login_token_response(nonce, client_id, subject, true)
}
