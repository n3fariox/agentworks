use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client, DynConnectTo};
use agentworks_runtime::acp;
use serde_json::json;
use std::path::PathBuf;

// Real ACP wire with a local fixture: discovery must never send a prompt or
// change a model, and authentication must follow the caller's explicit mode.
const FIXTURE: &str = r#"
import json,sys
mode=sys.argv[1]
expected_auth=sys.argv[2]=='yes'
authenticated=False
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');p=r.get('params',{})
 if m=='initialize':
  assert p['clientCapabilities']['terminal']==False
  assert p['clientCapabilities']['_meta']['terminal_output_delta']==True
  out={'protocolVersion':1,'agentCapabilities':{'sessionCapabilities':{'list':{}} if mode!='unsupported' else {}},'authMethods':[{'id':'api-key','name':'API key'}]}
 elif m=='authenticate':
  assert expected_auth and p['methodId']=='api-key'
  authenticated=True;out={}
 elif m=='session/new':
  assert mode=='models' and authenticated==expected_auth and p['cwd']=='/runtime-fixture'
  out={'sessionId':'temporary','configOptions':[{'id':'model','name':'Model','category':'model','type':'select','currentValue':'paid/model','options':[{'value':'paid/model','name':'Paid'},{'value':'opencode/model-free','name':'Free'}]}]}
 elif m=='session/list':
  assert mode=='list' and authenticated==expected_auth
  assert p['cwd']=='/runtime-fixture' and p['cursor']=='page-1'
  out={'sessions':[{'sessionId':'native','cwd':'/runtime-fixture','title':'Existing'}],'nextCursor':'page-2'}
 else: raise Exception('Unexpected ACP operation '+str(m))
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':out}),flush=True)
"#;

fn transport(mode: &str, authenticated: bool) -> DynConnectTo<Client> {
    DynConnectTo::new(AcpAgent::new(
        AcpAgentConfig::new("python3")
            .arg("-c")
            .arg(FIXTURE)
            .arg(mode)
            .arg(if authenticated { "yes" } else { "no" }),
    ))
}

#[tokio::test]
async fn model_discovery_filters_paid_models_without_prompting_or_changing_configuration() {
    let models = acp::discover(
        transport("models", false),
        "opencode",
        PathBuf::from("/runtime-fixture"),
        false,
    )
    .await
    .unwrap();
    assert_eq!(models["currentValue"], "opencode/model-free");
    assert_eq!(
        models["options"],
        json!([{"value":"opencode/model-free", "name":"Free"}])
    );
}

#[tokio::test]
async fn history_listing_preserves_scope_and_cursor_in_both_auth_modes() {
    for api_key in [false, true] {
        let listed = acp::list_sessions(
            transport("list", api_key),
            "codex",
            Some(PathBuf::from("/runtime-fixture")),
            Some("page-1".into()),
            api_key,
        )
        .await
        .unwrap();
        assert_eq!(listed["nextCursor"], "page-2");
        assert_eq!(listed["sessions"][0]["sessionId"], "native");
    }
}

#[tokio::test]
async fn history_listing_requires_the_advertised_capability() {
    let error = acp::list_sessions(transport("unsupported", false), "codex", None, None, false)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not advertise session listing")
    );
}
