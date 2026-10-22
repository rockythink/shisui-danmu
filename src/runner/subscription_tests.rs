use super::*;
use crate::runner::test_support::{HttpServer, Response as FixtureResponse, Scenario};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde_json::{Value, json};

pub(crate) fn fixture_session(
    config: &Path,
    origin: &str,
    expired: bool,
    direct_scope: bool,
) -> Result<Session> {
    let parsed = Url::parse(origin)?;
    ensure!(
        parsed.scheme() == "http"
            && parsed.host_str() == Some("127.0.0.1")
            && parsed.path() == "/"
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none(),
        "OAuth fixture must be loopback origin"
    );
    let origin = parsed.origin().ascii_serialization();
    let endpoints = Endpoints {
        authorize: format!("{origin}/authorize"),
        discovery: format!("{origin}/discovery"),
        token: format!("{origin}/token"),
        issuer: ISSUER.into(),
        test_origin: Some(origin),
    };
    let p = paths(config, true)?;
    let lock = lock_sync(&p)?;
    let mut store = read_store(&p)?.unwrap_or_else(|| Store {
        format: 1,
        host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
        accounts: Vec::new(),
    });
    let scope = if direct_scope {
        SCOPE
    } else {
        "openid profile email offline_access resource.invoke"
    };
    let response: TokenResponse = serde_json::from_value(token_response(
        "fixture-nonce",
        "oaiapp_fixture",
        "fixture-subject",
        scope,
        "refresh-1",
    ))?;
    let keys = serde_json::from_value(keys())?;
    let (mut tokens, scopes) = validate_tokens(
        response,
        &keys,
        &endpoints,
        "oaiapp_fixture",
        "fixture-subject",
        None,
    )?;
    if expired {
        tokens.expires_at = now().saturating_sub(1);
    }
    let registration = Registration {
        id: FIXTURE_ACCOUNT.into(),
        client_id: "oaiapp_fixture".into(),
        subject: "fixture-subject".into(),
        email: Some("fixture@example.test".into()),
        scopes,
        revision: 1,
        tokens: Some(tokens),
    };
    if let Some(existing) = store.accounts.iter_mut().find(|a| a.id == FIXTURE_ACCOUNT) {
        *existing = registration;
    } else {
        store.accounts.push(registration);
    }
    write_store(&p, &store)?;
    drop(lock);
    session_with(config, FIXTURE_ACCOUNT, endpoints)
}

fn ep(server: &HttpServer) -> Endpoints {
    Endpoints {
        authorize: server.url("/authorize"),
        discovery: server.url("/discovery"),
        token: server.url("/token"),
        issuer: ISSUER.into(),
        test_origin: Some(
            Url::parse(&server.url("/"))
                .unwrap()
                .origin()
                .ascii_serialization(),
        ),
    }
}
fn discover(ep: &Endpoints) -> Scenario {
    let origin = ep.test_origin.as_ref().unwrap();
    Scenario::new(FixtureResponse::json(
        json!({"issuer":ISSUER,"jwks_uri":format!("{origin}/jwks"),"token_endpoint":ep.token,"revocation_endpoint":format!("{origin}/revoke")}),
    ))
}
fn jwk_response() -> Scenario {
    Scenario::new(FixtureResponse::json(keys()))
}
fn query(login: &Login) -> BTreeMap<String, String> {
    Url::parse(login.url())
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}
async fn send_callback(login: &Login, suffix: &str) {
    let q = query(login);
    let url = format!("{}?state={}&{}", q["redirect_uri"], q["state"], suffix);
    let response = Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success() || response.status().as_u16() == 400);
}
pub(crate) async fn login_pending(
    config: &Path,
    server: &HttpServer,
    selected: Option<&str>,
    client: &str,
    subject: &str,
    scope: &str,
) -> PendingLogin {
    let endpoints = ep(server);
    let login = begin_login_with(config, selected, endpoints.clone()).unwrap();
    fixture_finish_login(login, server, client, subject, scope).await
}
pub(crate) async fn fixture_finish_login(
    login: Login,
    server: &HttpServer,
    client: &str,
    subject: &str,
    scope: &str,
) -> PendingLogin {
    let endpoints = ep(server);
    let q = query(&login);
    server.push(Scenario::respond({
        let q = q.clone();
        let client = client.to_owned();
        let subject = subject.to_owned();
        let scope = scope.to_owned();
        move |request| {
            let form: BTreeMap<_, _> = url::form_urlencoded::parse(&request.body)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            assert_eq!(request.path, "/token");
            assert_eq!(form["client_id"], client);
            assert_eq!(form["resource"], RESOURCE);
            assert_eq!(form["redirect_uri"], q["redirect_uri"]);
            assert_eq!(
                URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
                q["code_challenge"]
            );
            assert!(!form.contains_key("client_secret"));
            FixtureResponse::json(token_response(
                &q["nonce"],
                &client,
                &subject,
                &scope,
                "refresh-1",
            ))
        }
    }));
    server.push(discover(&endpoints));
    server.push(jwk_response());
    send_callback(&login, &format!("code=fixture-code&client_id={client}")).await;
    login.finish().await.unwrap()
}
fn config(root: &tempfile::TempDir) -> PathBuf {
    root.path().join("assistant.json")
}
fn age(config: &Path, id: &str) {
    let p = paths(config, false).unwrap();
    let _lock = lock_sync(&p).unwrap();
    let mut s = read_store(&p).unwrap().unwrap();
    s.accounts
        .iter_mut()
        .find(|a| a.id == id)
        .unwrap()
        .tokens
        .as_mut()
        .unwrap()
        .expires_at = now() + 1;
    write_store(&p, &s).unwrap();
}
#[tokio::test]
async fn login_is_pending_until_lifecycle_commit_and_scope_can_be_disabled() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let pending = login_pending(
        &config,
        &server,
        None,
        "oaiapp_one",
        "subject-one",
        "openid email offline_access resource.invoke",
    )
    .await;
    assert!(load_accounts(&config).unwrap().is_empty());
    let info = complete_login(&config, pending).unwrap();
    assert!(info.signed_in);
    assert!(!info.plan_enabled);
    assert!(session(&config, &info.id).is_err());
    server.shutdown().await;
}
#[tokio::test]
async fn returning_registration_and_multi_account_remain_isolated() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let one = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject-one", SCOPE).await,
    )
    .unwrap();
    let two = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_two", "subject-two", SCOPE).await,
    )
    .unwrap();
    assert_ne!(one.id, two.id);
    assert_ne!(one.label, two.label);
    assert_eq!(one.email, two.email);
    let login = begin_login_with(&config, Some(&one.id), ep(&server)).unwrap();
    let q = query(&login);
    assert_eq!(q["client_id"], "oaiapp_one");
    assert!(q.contains_key("id_token_hint"));
    assert!(!q.contains_key("agent_name_hint"));
    let host = q["ext_agent_host_id"].clone();
    drop(login);
    let pending = login_pending(
        &config,
        &server,
        Some(&one.id),
        "oaiapp_one",
        "subject-one",
        SCOPE,
    )
    .await;
    assert_eq!(pending.host_id, host);
    assert_eq!(complete_login(&config, pending).unwrap().id, one.id);
    assert_eq!(load_accounts(&config).unwrap().len(), 2);
    server.shutdown().await;
}
#[tokio::test]
async fn callback_duplicate_state_client_and_denial_never_exchange() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    for suffix in [
        "state=duplicate&code=x&client_id=oaiapp_one",
        "code=x&code=y&client_id=oaiapp_one",
        "code=x&client_id=dynamic_agent_client",
        "error=access_denied",
    ] {
        let login = begin_login_with(&config, None, ep(&server)).unwrap();
        send_callback(&login, suffix).await;
        assert!(login.finish().await.is_err());
    }
    let login = begin_login_with(&config, None, ep(&server)).unwrap();
    let q = query(&login);
    Client::new()
        .get(format!(
            "{}?state=wrong&code=x&client_id=oaiapp_one",
            q["redirect_uri"]
        ))
        .send()
        .await
        .unwrap();
    assert!(login.finish().await.is_err());
    assert!(server.requests().is_empty());
    server.shutdown().await;
}
#[tokio::test]
async fn wrong_nonce_and_returning_subject_are_rejected_without_replacing() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "original", SCOPE).await,
    )
    .unwrap();
    for (subject, nonce) in [("different", None), ("original", Some("wrong"))] {
        let endpoint = ep(&server);
        let login = begin_login_with(&config, Some(&info.id), endpoint.clone()).unwrap();
        let q = query(&login);
        server.push(Scenario::new(FixtureResponse::json(token_response(
            nonce.unwrap_or(&q["nonce"]),
            "oaiapp_one",
            subject,
            SCOPE,
            "other-refresh",
        ))));
        server.push(discover(&endpoint));
        server.push(jwk_response());
        send_callback(&login, "code=x&client_id=oaiapp_one").await;
        assert!(login.finish().await.is_err());
        assert!(load_accounts(&config).unwrap()[0].signed_in);
    }
    let login = begin_login_with(&config, Some(&info.id), ep(&server)).unwrap();
    send_callback(&login, "code=x&client_id=oaiapp_other").await;
    assert!(login.finish().await.is_err());
    server.shutdown().await;
}
#[test]
fn jwt_cryptographic_issuer_audience_expiry_and_algorithm_rejection() {
    let keys: JwkSet = serde_json::from_value(keys()).unwrap();
    let valid = json!({"iss":ISSUER,"aud":"oaiapp_one","sub":"subject","exp":now()+3600});
    assert!(verify(&signed(valid.clone()), &keys, ISSUER, "oaiapp_one").is_ok());
    for (field, value) in [
        ("iss", json!("https://attacker.test")),
        ("aud", json!("other-client")),
        ("exp", json!(now() - 1)),
        ("sub", json!("")),
    ] {
        let mut claims = valid.clone();
        claims[field] = value;
        assert!(verify(&signed(claims), &keys, ISSUER, "oaiapp_one").is_err());
    }
    let mut corrupt = signed(valid.clone()).into_bytes();
    let n = corrupt.len();
    corrupt[n - 10] = if corrupt[n - 10] == b'A' { b'B' } else { b'A' };
    assert!(
        verify(
            std::str::from_utf8(&corrupt).unwrap(),
            &keys,
            ISSUER,
            "oaiapp_one"
        )
        .is_err()
    );
    let mut h = Header::new(Algorithm::HS256);
    h.kid = Some("test-key".into());
    let forged = encode(&h, &valid, &EncodingKey::from_secret(b"fixture-secret")).unwrap();
    assert!(verify(&forged, &keys, ISSUER, "oaiapp_one").is_err());
    let mut duplicate: Value = serde_json::to_value(&keys).unwrap();
    let key = duplicate["keys"][0].clone();
    duplicate["keys"].as_array_mut().unwrap().push(key);
    assert!(
        verify(
            &signed(valid),
            &serde_json::from_value(duplicate).unwrap(),
            ISSUER,
            "oaiapp_one"
        )
        .is_err()
    );
}
#[tokio::test]
async fn cancelling_login_and_finish_releases_listener_and_task() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let login = begin_login_with(&config, None, ep(&server)).unwrap();
    let addr = query(&login)["redirect_uri"].clone();
    drop(login);
    tokio::task::yield_now().await;
    assert!(Client::new().get(addr).send().await.is_err());
    let login = begin_login_with(&config, None, ep(&server)).unwrap();
    let addr = query(&login)["redirect_uri"].clone();
    let task = tokio::spawn(login.finish());
    task.abort();
    let _ = task.await;
    tokio::task::yield_now().await;
    assert!(Client::new().get(addr).send().await.is_err());
    assert!(load_accounts(&config).unwrap().is_empty());
    server.shutdown().await;
}
#[tokio::test]
async fn refresh_rotation_is_serialized_and_headers_are_sensitive() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    age(&config, &info.id);
    let endpoint = ep(&server);
    server.push(discover(&endpoint));
    server.push(jwk_response());
    server.push(Scenario::respond(|r| {
        let form: BTreeMap<_, _> = url::form_urlencoded::parse(&r.body)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(form["refresh_token"], "refresh-1");
        assert!(!form.contains_key("scope"));
        FixtureResponse::json(token_response(
            "refresh-nonce",
            "oaiapp_one",
            "subject",
            SCOPE,
            "refresh-2",
        ))
    }));
    let mut first = session_with(&config, &info.id, endpoint.clone()).unwrap();
    let mut second = session_with(&config, &info.id, endpoint).unwrap();
    let (a, b) = tokio::join!(first.header(), second.header());
    assert!(a.unwrap().is_sensitive());
    assert!(b.unwrap().is_sensitive());
    let s = read_store(&paths(&config, false).unwrap())
        .unwrap()
        .unwrap();
    assert!(s.accounts[0].tokens.as_ref().unwrap().refresh_token == "refresh-2");
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.path == "/token")
            .count(),
        2
    );
    server.shutdown().await;
}
#[tokio::test]
async fn temporary_refresh_and_invalid_client_preserve_but_invalid_grant_clears_only_tokens() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    age(&config, &info.id);
    for code in [None, Some("invalid_client"), Some("invalid_grant")] {
        server.push(discover(&ep(&server)));
        server.push(jwk_response());
        server.push(Scenario::new(
            FixtureResponse::json(
                code.map(|c| json!({"error":c,"error_description":"private-sentinel"}))
                    .unwrap_or(json!({"detail":"private-sentinel"})),
            )
            .status(if code.is_none() { 503 } else { 400 }),
        ));
        let mut s = session_with(&config, &info.id, ep(&server)).unwrap();
        let error = s.header().await.err().unwrap();
        assert!(!format!("{error:?}").contains("private-sentinel"));
        assert_eq!(
            load_accounts(&config).unwrap()[0].signed_in,
            code != Some("invalid_grant")
        );
    }
    let login = begin_login_with(&config, Some(&info.id), ep(&server)).unwrap();
    let q = query(&login);
    assert_eq!(q["client_id"], "oaiapp_one");
    assert!(!q.contains_key("id_token_hint"));
    drop(login);
    server.shutdown().await;
}
#[tokio::test]
async fn signout_reports_confirmed_revocation_or_local_only_and_old_session_stops() {
    for status in [200, 503] {
        let root = tempfile::tempdir().unwrap();
        let config = config(&root);
        let server = HttpServer::spawn(vec![]).await;
        let info = complete_login(
            &config,
            login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
        )
        .unwrap();
        let other = complete_login(
            &config,
            login_pending(&config, &server, None, "oaiapp_two", "other-subject", SCOPE).await,
        )
        .unwrap();
        let mut old = session_with(&config, &info.id, ep(&server)).unwrap();
        server.push(discover(&ep(&server)));
        server.push(Scenario::respond(move |r| {
            assert_eq!(r.path, "/revoke");
            let form: BTreeMap<_, _> = url::form_urlencoded::parse(&r.body)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            assert_eq!(form["token"], "refresh-1");
            assert_eq!(form["token_type_hint"], "refresh_token");
            assert_eq!(form["client_id"], "oaiapp_one");
            FixtureResponse::bytes(Vec::new()).status(status)
        }));
        assert_eq!(
            sign_out_with(&config, &info.id, ep(&server))
                .await
                .unwrap()
                .remote_revoked,
            status == 200
        );
        assert!(old.header().await.is_err());
        assert_eq!(load_accounts(&config).unwrap()[0].id, info.id);
        assert!(!load_accounts(&config).unwrap()[0].signed_in);
        let mut unaffected = session_with(&config, &other.id, ep(&server)).unwrap();
        assert!(unaffected.header().await.unwrap().is_sensitive());
        server.shutdown().await;
    }
}
#[tokio::test]
async fn stale_login_completion_cannot_restore_signed_out_tokens() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    let pending = login_pending(
        &config,
        &server,
        Some(&info.id),
        "oaiapp_one",
        "subject",
        SCOPE,
    )
    .await;
    server.push(discover(&ep(&server)));
    server.push(Scenario::new(FixtureResponse::bytes(Vec::new())));
    sign_out_with(&config, &info.id, ep(&server)).await.unwrap();
    assert!(complete_login(&config, pending).is_err());
    assert!(!load_accounts(&config).unwrap()[0].signed_in);
    server.shutdown().await;
}
#[tokio::test]
async fn bounded_and_wrong_discovery_never_accept_credentials() {
    let server = HttpServer::spawn(vec![]).await;
    let endpoint = ep(&server);
    let http = client(&endpoint).unwrap();
    server.push(Scenario::new(FixtureResponse::chunked(vec![
        vec![b'x'; MAX_HTTP],
        vec![b'x'; 1],
    ])));
    assert!(matches!(
        discovery(&http, &endpoint).await,
        Err(SubscriptionError::Protocol(_))
    ));
    server.push(Scenario::new(FixtureResponse::json(json!({"issuer":ISSUER,"token_endpoint":endpoint.token,"jwks_uri":"https://attacker.test/jwks","revocation_endpoint":"https://attacker.test/revoke"}))));
    assert!(discovery(&http, &endpoint).await.is_err());
    server.shutdown().await;
}
#[cfg(unix)]
#[tokio::test]
async fn private_storage_rejects_symlinks_permissions_hardlinks_and_failed_commit() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let pending = login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await;
    let p = paths(&config, false).unwrap();
    let original = std::fs::read(&p.file).unwrap();
    std::fs::set_permissions(&p.file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load_accounts(&config).is_err());
    assert!(complete_login(&config, pending).is_err());
    assert_eq!(std::fs::read(&p.file).unwrap(), original);
    std::fs::set_permissions(&p.file, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&p.file, root.path().join("alias")).unwrap();
    assert!(load_accounts(&config).is_err());
    std::fs::remove_file(root.path().join("alias")).unwrap();
    std::fs::rename(&p.file, root.path().join("saved")).unwrap();
    symlink(root.path().join("saved"), &p.file).unwrap();
    assert!(load_accounts(&config).is_err());
    std::fs::remove_file(&p.file).unwrap();
    std::fs::rename(root.path().join("saved"), &p.file).unwrap();
    std::fs::set_permissions(&p.root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(load_accounts(&config).is_err());
    std::fs::set_permissions(&p.root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(load_accounts(&config).is_err());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn earliest_refresh_time_is_respected_and_expiry_is_never_ignored() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    let p = paths(&config, false).unwrap();
    let mut store = read_store(&p).unwrap().unwrap();
    let t = store.accounts[0].tokens.as_mut().unwrap();
    t.expires_at = now() + 30;
    t.earliest_refresh_at = Some(now() + 300);
    write_store(&p, &store).unwrap();
    let mut s = session_with(&config, &info.id, ep(&server)).unwrap();
    assert!(s.header().await.is_ok());
    store.accounts[0].tokens.as_mut().unwrap().expires_at = now() - 1;
    write_store(&p, &store).unwrap();
    assert!(s.header().await.is_err());
    assert_eq!(server.requests().len(), 3);
    assert!(load_accounts(&config).unwrap()[0].signed_in);
    server.shutdown().await;
}
#[tokio::test]
async fn cancellation_before_refresh_exchange_preserves_rotation_and_releases_lock() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    age(&config, &info.id);
    let endpoint = ep(&server);
    server.push(discover(&endpoint));
    let (held, received, release) = jwk_response().barrier();
    server.push(held);
    let mut session = session_with(&config, &info.id, endpoint.clone()).unwrap();
    let task = tokio::spawn(async move { session.header().await });
    received.await.unwrap();
    task.abort();
    let _ = task.await;
    let _ = release.send(());
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.path == "/token")
            .count(),
        1
    );
    let p = paths(&config, false).unwrap();
    assert!(
        read_store(&p).unwrap().unwrap().accounts[0]
            .tokens
            .as_ref()
            .unwrap()
            .refresh_token
            == "refresh-1"
    );
    server.push(discover(&endpoint));
    server.push(jwk_response());
    let (held, received, release) = Scenario::new(FixtureResponse::json(token_response(
        "n",
        "oaiapp_one",
        "subject",
        SCOPE,
        "refresh-2",
    )))
    .barrier();
    server.push(held);
    let mut session = session_with(&config, &info.id, endpoint).unwrap();
    let task = tokio::spawn(async move { session.header().await });
    received.await.unwrap();
    release.send(()).unwrap();
    assert!(task.await.unwrap().unwrap().is_sensitive());
    assert!(
        read_store(&p).unwrap().unwrap().accounts[0]
            .tokens
            .as_ref()
            .unwrap()
            .refresh_token
            == "refresh-2"
    );
    server.shutdown().await;
}
#[tokio::test]
async fn cancellation_of_revocation_leaves_local_tokens_cleared() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    let endpoint = ep(&server);
    server.push(discover(&endpoint));
    let (held, received, release) = Scenario::new(FixtureResponse::bytes(Vec::new())).barrier();
    server.push(held);
    let task = tokio::spawn({
        let config = config.clone();
        let id = info.id.clone();
        async move { sign_out_with(&config, &id, endpoint).await }
    });
    received.await.unwrap();
    assert!(!load_accounts(&config).unwrap()[0].signed_in);
    task.abort();
    let _ = task.await;
    let _ = release.send(());
    assert!(session_with(&config, &info.id, ep(&server)).is_err());
    server.shutdown().await;
}
#[cfg(unix)]
#[tokio::test]
async fn actual_storage_failure_preserves_previous_record_and_signout_reports_error() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    let p = paths(&config, false).unwrap();
    let original = std::fs::read(&p.file).unwrap();
    std::fs::set_permissions(&p.root, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = sign_out_with(&config, &info.id, ep(&server)).await;
    std::fs::set_permissions(&p.root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(&p.file).unwrap(), original);
    assert_eq!(server.requests().len(), 3);
    assert!(load_accounts(&config).unwrap()[0].signed_in);
    server.shutdown().await;
}
#[cfg(unix)]
#[tokio::test]
async fn refreshed_tokens_are_not_exposed_when_durable_replacement_fails() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    age(&config, &info.id);
    let p = paths(&config, false).unwrap();
    let original = std::fs::read(&p.file).unwrap();
    let endpoint = ep(&server);
    server.push(discover(&endpoint));
    server.push(jwk_response());
    server.push(Scenario::respond({
        let file = p.file.clone();
        move |_| {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            FixtureResponse::json(token_response(
                "n",
                "oaiapp_one",
                "subject",
                SCOPE,
                "replacement-private-sentinel",
            ))
        }
    }));
    let mut s = session_with(&config, &info.id, endpoint).unwrap();
    let error = s.header().await.err().unwrap();
    assert!(!format!("{error:?}").contains("replacement-private-sentinel"));
    assert_eq!(std::fs::read(&p.file).unwrap(), original);
    std::fs::set_permissions(&p.file, std::fs::Permissions::from_mode(0o600)).unwrap();
    server.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_rotation_across_processes() {
    const PHASE: &str = "DANMU_TEST_SUBSCRIPTION_PHASE";
    if let Ok(phase) = std::env::var(PHASE) {
        assert_eq!(phase, "reader");
        let config = PathBuf::from(std::env::var("DANMU_TEST_SUBSCRIPTION_CONFIG").unwrap());
        let id = std::env::var("DANMU_TEST_SUBSCRIPTION_ACCOUNT").unwrap();
        let origin = std::env::var("DANMU_TEST_SUBSCRIPTION_ORIGIN").unwrap();
        let parsed = Url::parse(&origin).unwrap();
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        let endpoints = Endpoints {
            authorize: format!("{origin}/authorize"),
            discovery: format!("{origin}/discovery"),
            token: format!("{origin}/token"),
            issuer: ISSUER.into(),
            test_origin: Some(origin),
        };
        let mut session = session_with(&config, &id, endpoints).unwrap();
        assert!(session.header().await.unwrap().is_sensitive());
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let server = HttpServer::spawn(vec![]).await;
    let info = complete_login(
        &config,
        login_pending(&config, &server, None, "oaiapp_one", "subject", SCOPE).await,
    )
    .unwrap();
    age(&config, &info.id);
    let endpoint = ep(&server);
    server.push(discover(&endpoint));
    server.push(jwk_response());
    server.push(Scenario::respond(|r| {
        let form: BTreeMap<_, _> = url::form_urlencoded::parse(&r.body)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(form["refresh_token"], "refresh-1");
        FixtureResponse::json(token_response(
            "n",
            "oaiapp_one",
            "subject",
            SCOPE,
            "refresh-2",
        ))
    }));
    let mut children = Vec::new();
    for _ in 0..2 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runner::subscription::tests::refresh_rotation_across_processes",
                    "--nocapture",
                ])
                .env(PHASE, "reader")
                .env("DANMU_TEST_SUBSCRIPTION_CONFIG", &config)
                .env("DANMU_TEST_SUBSCRIPTION_ACCOUNT", &info.id)
                .env(
                    "DANMU_TEST_SUBSCRIPTION_ORIGIN",
                    endpoint.test_origin.as_ref().unwrap(),
                )
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.path == "/token")
            .count(),
        2
    );
    let p = paths(&config, false).unwrap();
    assert!(
        read_store(&p).unwrap().unwrap().accounts[0]
            .tokens
            .as_ref()
            .unwrap()
            .refresh_token
            == "refresh-2"
    );
    server.shutdown().await;
}
#[test]
fn corrupt_bounded_and_unknown_private_records_are_never_treated_as_signed_out() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let p = paths(&config, true).unwrap();
    for bytes in [b"not-json".to_vec(),vec![b'x';MAX_FILE+1],serde_json::to_vec(&json!({"format":2,"host_id":format!("urn:uuid:{}",uuid::Uuid::new_v4()),"accounts":[]})).unwrap()] {
        crate::storage::write_private_atomic(&p.file,&bytes).unwrap();
        assert!(load_accounts(&config).is_err()); assert_eq!(std::fs::read(&p.file).unwrap(),bytes);
    }
}

#[test]
fn access_tokens_require_subject_client_resource_and_granted_scope_binding() {
    let keys: JwkSet = serde_json::from_value(keys()).unwrap();
    let ep = Endpoints::official();
    for (field, value) in [
        ("sub", json!("other")),
        ("client_id", json!("other-client")),
        ("aud", json!("https://other.test")),
        ("scope", json!("openid")),
    ] {
        let mut access = json!({"iss":ISSUER,"aud":RESOURCE,"sub":"subject","client_id":"oaiapp_one","scope":SCOPE,"exp":now()+3600});
        access[field] = value;
        let mut response = token_response("n", "oaiapp_one", "subject", SCOPE, "refresh");
        response["access_token"] = json!(signed(access));
        let t: TokenResponse = serde_json::from_value(response).unwrap();
        assert!(validate_tokens(t, &keys, &ep, "oaiapp_one", "subject", None).is_err());
    }
    let multi = json!({"iss":ISSUER,"aud":["oaiapp_one","other"],"sub":"subject","exp":now()+3600});
    assert!(verify(&signed(multi), &keys, ISSUER, "oaiapp_one").is_err());
}
#[test]
fn callback_requires_exact_redirect_and_returning_client_even_on_denial() {
    let a = Attempt {
        host_id: "host".into(),
        id: "id".into(),
        revision: Some(1),
        client_id: "oaiapp_one".into(),
        subject: Some("subject".into()),
        nonce: "nonce".into(),
        state: "state".into(),
        verifier: "verifier".into(),
        redirect: "http://127.0.0.1:12345/auth/callback".into(),
    };
    assert!(
        callback(
            "/auth/callback?state=state&code=code",
            "127.0.0.1:12345",
            &a
        )
        .is_ok()
    );
    for (target, host) in [
        ("/wrong?state=state&code=code", "127.0.0.1:12345"),
        ("/auth/callback?state=state&code=code", "localhost:12345"),
        (
            "/auth/callback?state=wrong&error=access_denied",
            "127.0.0.1:12345",
        ),
        (
            "/auth/callback?state=state&code=code&client_id=other",
            "127.0.0.1:12345",
        ),
        (
            "/auth/callback?state=state&code=code#fragment",
            "127.0.0.1:12345",
        ),
    ] {
        assert!(callback(target, host, &a).is_err());
    }
}
