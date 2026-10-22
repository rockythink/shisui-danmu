use super::*;
use jsonwebtoken::{EncodingKey, Header, encode};
use serde_json::{Value, json};
// Generated RSA key belongs only to deterministic loopback tests, never a credential.
const TEST_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAnqdyns2Ep64Jn2vou0o7oAS6S/7qODO6SJrLqLOQ4EjKoT9a\nAmM3kuOhTgfk54TosPXhelFbZ0vCCrZjLT99E1ZHEgmO6lSqnTQ3OipLQCcnUiN5\nGwHOkZRUJZbkpQ/T2tug0F1j7euPXiwWEycKXZOkRb1HS5UDK2qZFGOYUqpgExXF\nOLkRlkBtR3vC6OkgNR2BvYbQzBMYjnYjqS+N936VYx5nvLkboBjeuxmV6787IgBh\nH1wtb2WeI6/AtmmQMeX3xlnt+CR5RIOBq0VM78QgQ+g0FRObHdsUOsIaLYuT6CVS\nrMTIGBsF3jJ7wbzz7Ivbuu3lzHbQEGF2T9ma+QIDAQABAoIBAAlry2xPVfDtw8KE\nvOWKQJooTbZqOmxV81o7tEZIoMdrayTsxyKHKRX+5brlexvzEiztR5Nk4Lb0kl7i\nemcwcvPtVqZlN5MGA2T3DyR+vciJyqr0/PcJQhlLo+eLhfj171BQDxpFwW5aY5a7\nACCTsT43SzTySsPb4xFL8Np7bjXSXOyH0FQWrjVnOSDKSjTKANP6LJePQl91QhFf\n9Mmwlu8icyDwpW6ezuNHTwbN29bgiobXhr3v4hi7v7FAnVPAh10Wg5V8zXOJXYXP\n03bJb63QXvIvhWMnRxJW8OYemmfzAnnp5ZLzb/IByedszgdDwA0qxG86eZ6Ely0z\nRqdQJYUCgYEAy3wED1UDn0OwaSaY3ZytcHxLfM74ZS3qVWOYhuTn8qBk66yZ91Qs\nVqmvxxGRYzEpSiRa3PZeLZ17zKOGL6CcfWfK+gd5gIw5019iStyzO5dt0hhteUdH\nr25ZLusqe7DYOrmfWj2ZL5ylrd3O1pJGeWLzfojNJf/N53k1W2bSqJsCgYEAx5mK\n0tyakPME8NqEMT+W3OpJueodaXzPcLOUhLEoAzSgm4W44uqmaabpWLDc/f1I/oRB\nDSF5UMzm9jTo8/mApkZrM1XoXn0enJB3w7bwixcfU+ZsADP0mkuKXapQ3otBO7HX\nug+4cNo+xde7w0PTaEFx152ZW9sS9/QMTGIUEfsCgYEAk9xL/vAhr6s6F05qqi5d\ngW1wVQL4UF1q+B1lCjBvs0iFzRMg1i4eAeNPzXGgmdlxTWTZedK0YKUc/nic2yL0\nU9QGFGU2DwjWRBf9jrSMsPK6ZH5HnmcDWiDvQT61IihBF3smpiY4l/ZWFd8lm5mz\nCCW1Fe9GUyRsdJUJBp/S150CgYAvCUvhrQLvrZU4fQYx5N5UaRG+siEEPtlTob4a\nFvVq6/f9LmIHd2yH8UGEGQm7APk2oUwDICRKmj7DPmy3MdDTXjzc+B2uRqX0F0dh\n47aD0IsdZmuFELlPswWEVSKBjydsPdisjA/W6FdqoBArT8x+AQ4m1YjSUKoDrItJ\nZMN+CQKBgHYCf9iTPDW1/K66Td27VHmQHV8vPa4mnlUhdE53GNdR6wQ28S7gAwUu\nvkyGNwoHLMV8AXXgm1bLYnKjUtSDdEytr41Upgxo0ICuKzyRHTe/Bn5U0xy27HXc\nulJsVjubKwirHB7ZW6ZFJE3dnsi0H1EmrLorzulmZVMGcp+HUnG1\n-----END RSA PRIVATE KEY-----\n";
const TEST_N: &str = "nqdyns2Ep64Jn2vou0o7oAS6S_7qODO6SJrLqLOQ4EjKoT9aAmM3kuOhTgfk54TosPXhelFbZ0vCCrZjLT99E1ZHEgmO6lSqnTQ3OipLQCcnUiN5GwHOkZRUJZbkpQ_T2tug0F1j7euPXiwWEycKXZOkRb1HS5UDK2qZFGOYUqpgExXFOLkRlkBtR3vC6OkgNR2BvYbQzBMYjnYjqS-N936VYx5nvLkboBjeuxmV6787IgBhH1wtb2WeI6_AtmmQMeX3xlnt-CR5RIOBq0VM78QgQ-g0FRObHdsUOsIaLYuT6CVSrMTIGBsF3jJ7wbzz7Ivbuu3lzHbQEGF2T9ma-Q";

// Shared signed OAuth fixture for the unified HTTP worker, still absent in production.
pub(crate) const FIXTURE_ACCOUNT: &str = "32c77c30-0d4a-4b26-8d06-566558a22127";
pub(crate) fn fixture_jwks() -> Value {
    keys()
}
pub(crate) fn fixture_discovery(origin: &str) -> Value {
    json!({"issuer":ISSUER,"jwks_uri":format!("{origin}/jwks"),"token_endpoint":format!("{origin}/token"),"revocation_endpoint":format!("{origin}/revoke")})
}
pub(crate) fn fixture_token_response(refresh_token: &str) -> Value {
    token_response(
        "fixture-nonce",
        "oaiapp_fixture",
        "fixture-subject",
        SCOPE,
        refresh_token,
    )
}
pub(crate) fn fixture_login_token_response(
    nonce: &str,
    issued: &str,
    subject: &str,
    direct_scope: bool,
) -> Value {
    let scope = if direct_scope {
        SCOPE
    } else {
        "openid profile email offline_access resource.invoke"
    };
    token_response(nonce, issued, subject, scope, "refresh-1")
}
pub(super) fn keys() -> Value {
    json!({"keys":[{"kty":"RSA","kid":"test-key","use":"sig","alg":"RS256","n":TEST_N,"e":"AQAB"}]})
}
pub(super) fn signed(claims: Value) -> String {
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some("test-key".into());
    let raw = TEST_KEY
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    let der = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .unwrap();
    encode(&h, &claims, &EncodingKey::from_rsa_der(&der)).unwrap()
}
pub(super) fn token_response(
    nonce: &str,
    client: &str,
    subject: &str,
    scope: &str,
    refresh: &str,
) -> Value {
    json!({"token_type":"Bearer","expires_in":3600,"scope":scope,"refresh_token":refresh,
      "id_token":signed(json!({"iss":ISSUER,"aud":client,"sub":subject,"nonce":nonce,"email":"fixture@example.test","exp":now()+3600})),
      "access_token":signed(json!({"iss":ISSUER,"aud":RESOURCE,"sub":subject,"client_id":client,"scope":scope,"exp":now()+3600}))})
}
