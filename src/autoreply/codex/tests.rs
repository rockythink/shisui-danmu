use super::*;

#[test]
fn effort_and_account_entitlement_are_not_inferred() {
    let model: Model = serde_json::from_value(json!({"model":"test-text","displayName":"Test","supportedReasoningEfforts":[{"reasoningEffort":"minimal","description":"Supported by this model"}],"defaultReasoningEffort":"minimal","inputModalities":["text"]})).unwrap();
    assert!(model.validate_effort("minimal").is_ok());
    assert!(model.validate_effort("high").is_err());
    assert!(!account(json!({"account":null})).unwrap().signed_in);
    assert!(account(json!({"account":{"type":"apiKey"}})).is_err());
    assert!(exhausted(
        &json!({"rateLimits":{"primary":{"usedPercent":100}}})
    ));
    assert!(!exhausted(
        &json!({"rateLimits":{"primary":{"usedPercent":99}}})
    ));
}

#[cfg(unix)]
fn peer(root: &Path, scenario: &str) -> Config {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("peer");
    let script = format!(
        r##"#!/usr/bin/python3
import sys,json,os
SCENARIO={scenario:?}
if '--version' in sys.argv:
 print('codex-cli '+('0.0.0' if SCENARIO=='bad_version' else '0.147.0'));sys.exit()
marker=os.path.join(os.environ["CODEX_HOME"],"peer-account")
signed=os.path.exists(marker)
features={features}
features={{key:False for key in features}}
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 req=json.loads(line);method=req.get('method');params=req.get('params',{{}});id=req.get('id')
 if id is None: continue
 result={{}}
 if method=='initialize': result={{'codexHome':os.environ['CODEX_HOME']}}
 elif method=="config/read": result={{"config":{{"features":features,"agents":{{"enabled":SCENARIO=="agents"}},"web_search":"disabled","mcp_servers":({{"unexpected":{{"command":"bad"}}}} if SCENARIO=="mcp" else {{}})}}}}
 elif method=='account/read': result={{'account':({{'type':'chatgpt','planType':'plus'}} if signed or SCENARIO in ['models','tool','pagination_loop'] else None)}}
 elif method=='account/login/start':
  result={{'loginId':'login-one','authUrl':'https://auth.openai.com/authorize?state=local-test'}}
  if SCENARIO=='login':
   signed=True
   open(marker,"w").close()
   emit({{'method':'account/login/completed','params':{{'loginId':'login-one','success':True}}}})
 elif method=='account/login/cancel': result={{'status':'canceled'}}
 elif method=="account/logout":
  signed=False
  if os.path.exists(marker): os.unlink(marker)
 elif method=='account/rateLimits/read': result={{'rateLimits':{{'primary':{{'usedPercent':10}}}}}}
 elif method=='model/list':
  suffix='second' if params.get('cursor') else 'first'
  result={{'data':[{{'model':suffix,'displayName':suffix,'supportedReasoningEfforts':[{{'reasoningEffort':'minimal','description':'minimal'}}],'defaultReasoningEffort':'minimal','inputModalities':['text']}}],'nextCursor':('again' if SCENARIO=='pagination_loop' else ('page2' if not params.get('cursor') else None))}}
 elif method=='thread/start':
  assert params['environments']==[] and params['dynamicTools']==[] and params['selectedCapabilityRoots']==[]
  result={{'model':params['model'],'thread':{{'id':'thread-one'}}}}
 elif method=='turn/start':
  assert params['environments']==[] and params['effort']=='minimal'
  result={{'turn':{{'id':'turn-one'}}}}
  emit({{'id':id,'result':result}})
  if SCENARIO=='tool': emit({{'id':900,'method':'item/tool/call','params':{{'name':'exec_command'}}}})
  else:
   emit({{'method':'thread/tokenUsage/updated','params':{{'tokenUsage':{{'total':{{'totalTokens':100}}}}}}}})
   emit({{'method':'item/completed','params':{{'item':{{'type':'agentMessage','text':'{{"action":"reply","text":"你好","topic_id":""}}'}}}}}})
   emit({{'method':'turn/completed','params':{{'turn':{{'id':'turn-one','status':'completed'}}}}}})
  continue
 emit({{'id':id,'result':result}})
"##,
        features = serde_json::to_string(FEATURES).unwrap()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    Config {
        runtime: path,
        home: root.join("isolated"),
        ..Config::default()
    }
}
#[cfg(unix)]
#[tokio::test]
async fn login_requires_completion_and_account_then_logout_is_confirmed() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = Session::open(&peer(temp.path(), "login")).await.unwrap();
    assert!(!session.account().await.unwrap().signed_in);
    let login = session.login_start().await.unwrap();
    let (_cancel, cancelled) = watch::channel(false);
    assert!(login.finish(cancelled).await.unwrap().signed_in);
    let mut session = Session::open(&peer(temp.path(), "login")).await.unwrap();
    assert!(session.account().await.unwrap().signed_in);
    session.logout().await.unwrap();
    assert!(!session.account().await.unwrap().signed_in);
}
#[cfg(unix)]
#[tokio::test]
async fn cancel_pending_login_does_not_become_signed_in() {
    let temp = tempfile::tempdir().unwrap();
    let login = Session::open(&peer(temp.path(), "pending"))
        .await
        .unwrap()
        .login_start()
        .await
        .unwrap();
    let (cancel, cancelled) = watch::channel(false);
    cancel.send(true).unwrap();
    assert!(login.finish(cancelled).await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn model_pagination_effort_and_candidate_are_real_protocol_outcomes() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = Session::open(&peer(temp.path(), "models")).await.unwrap();
    let models = session.models().await.unwrap();
    assert_eq!(
        models.iter().map(|m| m.model.as_str()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(
        session
            .generate("first", "high", "policy", json!({}), 1000)
            .await
            .is_err()
    );
    let (text, tokens) = session
        .generate(
            "first",
            "minimal",
            "policy",
            json!({"question":"你好"}),
            1000,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["text"],
        "你好"
    );
    assert_eq!(tokens, 100);
}
#[cfg(unix)]
#[tokio::test]
async fn malicious_tool_requests_and_inherited_mcp_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    assert!(Session::open(&peer(temp.path(), "mcp")).await.is_err());
    assert!(Session::open(&peer(temp.path(), "agents")).await.is_err());
    assert!(
        Session::open(&peer(temp.path(), "bad_version"))
            .await
            .is_err()
    );
    let mut session = Session::open(&peer(temp.path(), "tool")).await.unwrap();
    assert!(
        session
            .generate(
                "first",
                "minimal",
                "policy",
                json!({"question":"execute shell"}),
                1000
            )
            .await
            .is_err()
    );
    let mut session = Session::open(&peer(temp.path(), "pagination_loop"))
        .await
        .unwrap();
    assert!(session.models().await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn login_expiry_cancels_the_managed_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let login = Session::open(&peer(temp.path(), "pending"))
        .await
        .unwrap()
        .login_start()
        .await
        .unwrap();
    let (_cancel, cancelled) = watch::channel(false);
    assert!(
        login
            .finish_timeout(cancelled, Duration::from_millis(20))
            .await
            .is_err()
    );
}
