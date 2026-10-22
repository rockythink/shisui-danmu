"""Explicit, import-safe loopback fixtures shared by TUI and desktop QA."""
import http.server
import json
import os
from pathlib import Path
import queue
import threading
import urllib.parse

FAKE_KEY = 'smoke-key-not-a-real-credential-93fd71'
FAILED_KEY = 'smoke-unsaved-key-42be19'

def private_json(path, *, secrets):
    assert path.stat().st_mode & 0o777 == 0o600, 'private fixture mode'
    value = json.loads(path.read_text())
    def collect(node):
        if isinstance(node,dict):
            for key,val in node.items():
                if key in ['access_token','refresh_token','id_token'] and isinstance(val,str): secrets.add(val)
                collect(val)
        elif isinstance(node,list):
            for val in node: collect(val)
    collect(value)
    return value

def private_write(path,value):
    data = json.dumps(value).encode()
    fd = os.open(path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
    with os.fdopen(fd,'wb') as f: f.write(data)


class Scenario:
    def __init__(self, name, reply='', kind='answer', previous=None):
        self.name, self.reply, self.kind, self.previous = name, reply, kind, previous
        self.received = threading.Event()
        self.release = threading.Event()
        self.finished = threading.Event()
        self.round_id = self.message_id = None
        self.search_id = 'smoke-search-call'

class ModelDriver:
    def __init__(self, *, protocol, private_root, fixture_secrets):
        if protocol not in ('openai_chat', 'anthropic_messages', 'chatgpt_subscription'):
            raise ValueError('unsupported smoke protocol')
        private_root = Path(private_root)
        if not private_root.is_absolute() or not private_root.is_dir():
            raise ValueError('private fixture root must be an existing absolute directory')
        self.protocol = protocol
        self.fixture_file = private_root / 'browser.fixtures.json'
        self.browser_channel = private_root / 'browser.json'
        self.read_private = lambda path: private_json(path, secrets=fixture_secrets)
        self.pending = queue.Queue()
        self.requests = []
        self.failures = []
        self.lock = threading.Lock()
        self.search = None
        self.auth_calls = []
        self.catalog_count = 0
        self.revoke_status = 200
        self.large_stream_bytes = 0
        owner = self
        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *args): pass
            def send_json(self,value,status=200):
                data = json.dumps(value).encode()
                self.send_response(status);self.send_header('Content-Type','application/json')
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.close_connection = True
                self.end_headers()
                self.wfile.write(data)
            def subscription_authorized(self):
                values = owner.read_private(owner.fixture_file)
                tokens = [values['refresh']['access_token']]
                if owner.browser_channel.exists(): tokens.append(owner.read_private(owner.browser_channel)['token_response']['access_token'])
                assert self.headers.get('Authorization') in ['Bearer '+v for v in tokens], 'subscription auth mismatch'
                assert not self.headers.get('x-api-key'), 'subscription reused API key'
            def do_GET(self):
                try:
                    assert owner.protocol == 'chatgpt_subscription', 'unexpected GET'
                    if self.path == '/discovery': self.send_json(owner.read_private(owner.fixture_file)['discovery'])
                    elif self.path == '/jwks': self.send_json(owner.read_private(owner.fixture_file)['jwks'])
                    elif self.path == '/v1/models':
                        self.subscription_authorized();owner.catalog_count += 1
                        catalog = {"models": [
                            {"slug": "smoke-model", "display_name": "SMOKE Model", "visibility": "list",
                             "unused_metadata": {"description": "catalog-metadata-" * 40000}},
                            {"slug": "second-model", "display_name": "SECOND Model", "visibility": "list"},
                            {"slug": "not-selectable", "display_name": "Hidden", "visibility": "hidden"}]}
                        assert len(json.dumps(catalog).encode()) > 256 * 1024
                        self.send_json(catalog)
                    else: raise AssertionError('unexpected subscription GET route')
                except Exception as error:
                    owner.failures.append(type(error).__name__+': '+str(error))
                    self.send_json({},500)
            def do_POST(self):
                if owner.protocol == 'chatgpt_subscription' and self.path in ['/token','/revoke']:
                    try:
                        form = urllib.parse.parse_qs(self.rfile.read(int(self.headers['Content-Length'])).decode())
                        if self.path == '/revoke':
                            owner.auth_calls.append('revoke')
                            self.send_json({},owner.revoke_status)
                        elif form.get('grant_type') == ['authorization_code']:
                            owner.auth_calls.append('authorization_code')
                            assert form.get('code') == ['fixture-code'] and form.get('client_id') == ['oaiapp_fixture'], 'code binding'
                            assert len(form.get('code_verifier',[''])[0]) >= 43, 'PKCE missing'
                            self.send_json(owner.read_private(owner.browser_channel)['token_response'])
                        else:
                            assert form.get('grant_type') == ['refresh_token'], 'unexpected grant'
                            owner.auth_calls.append('refresh_token')
                            self.send_json(owner.read_private(owner.fixture_file)['refresh'])
                    except Exception as error:
                        owner.failures.append(type(error).__name__+': '+str(error));self.send_json({},500)
                    return
                scenario = None
                try:
                    expected = {'openai_chat':'/v1/chat/completions','anthropic_messages':'/v1/messages','chatgpt_subscription':'/v1/responses'}[owner.protocol]
                    assert self.path == expected, 'unexpected model route'
                    assert int(self.headers.get('Content-Length','0')) <= 48 * 1024, 'wire budget'
                    if owner.protocol == 'openai_chat':
                        assert self.headers.get('Authorization') == 'Bearer ' + FAKE_KEY, 'auth mismatch'
                    elif owner.protocol == 'chatgpt_subscription':
                        self.subscription_authorized()
                    else:
                        assert self.headers.get('x-api-key') == FAKE_KEY, 'auth mismatch'
                        assert self.headers.get('anthropic-version') == '2023-06-01', 'version mismatch'
                        assert self.headers.get('Authorization') is None, 'cross-protocol auth'
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    assert body['model'] == 'smoke-model', 'requested model'
                    if owner.protocol == 'chatgpt_subscription':
                        assert body['store'] is False and body['stream'] is True, 'subscription inference contract'
                        assert 'instructions' in body and not any(k in body for k in ['messages','max_output_tokens','previous_response_id','temperature']), 'unsupported subscription fields'
                    follow_search = owner.search is not None
                    if follow_search:
                        scenario = owner.search
                        owner.search = None
                        assert not body.get('tools'), 'second completion must have no tools'
                        serialized = json.dumps(body,ensure_ascii=False)
                        assert '不可用' in serialized or '未核实' in serialized, 'unavailable search result missing'
                        payload = next(json.loads(m['content']) for m in body.get('messages',body.get('input',[]))
                            if m.get('role') == 'user' and isinstance(m.get('content'),str))
                    else:
                        scenario = owner.pending.get_nowait()
                        payload = json.loads(body.get('messages',body.get('input',[]))[0 if owner.protocol == 'chatgpt_subscription' else -1]['content'])
                        scenario.round_id = payload['round_id']
                        scenario.message_id = payload['untrusted_messages'][0]['id']
                        assert payload['untrusted_messages'], 'empty generation round'
                        assert 'operator_context' in payload and 'actual_delivery_results' in payload, 'incomplete local context'
                        if scenario.previous:
                            previous = json.dumps({k:v for k,v in payload.items() if k != 'untrusted_messages'},ensure_ascii=False)
                            assert scenario.previous in previous, 'followup lost prior local context'
                            delivered = payload['actual_delivery_results']
                            assert any(r['state'] == 'confirmed' and r['confirmed_segments'] for r in delivered), 'no actual confirmed delivery context'
                    with owner.lock:
                        evidence = dict(payload)
                        evidence['operator_context'] = '[synthetic local references omitted]'
                        owner.requests.append({'scenario':scenario.name,'search_continuation':follow_search,
                            'authenticated':True,'payload':evidence,'tools_present':bool(body.get('tools'))})
                    scenario.received.set()
                    if scenario.kind == 'quota':
                        self.send_json({'error':{'code':'subscription_sharing_usage_limit_exceeded','message':'UPSTREAM_SECRET_SHOULD_NOT_APPEAR'}},429)
                        scenario.finished.set();return
                    if scenario.kind == 'hold':
                        assert scenario.release.wait(30), 'hold barrier expired'
                    text = json.dumps({'round_id':scenario.round_id,'candidates':[
                        {'message_id':scenario.message_id,'text':scenario.reply}]},ensure_ascii=False)
                    if scenario.kind == 'bad_json': text = '{invalid candidate JSON'
                    if scenario.kind == 'search' and not follow_search:
                        assert body.get('tools'), 'search not offered'
                        owner.search = scenario
                        args = json.dumps({'message_id':scenario.message_id})
                        if owner.protocol == 'openai_chat':
                            message = {'role':'assistant','content':None,'tool_calls':[{'id':scenario.search_id,
                                'type':'function','function':{'name':'web_search','arguments':args}}]}
                            response = {'model':'smoke-returned','choices':[{'index':0,'message':message,'finish_reason':'tool_calls'}]}
                        elif owner.protocol == 'chatgpt_subscription':
                            response = {'id':'resp-smoke','object':'response','status':'completed','model':'smoke-returned','output':[
                                {'type':'function_call','namespace':'shisui_danmu','name':'web_search','call_id':scenario.search_id,
                                 'arguments':args,'status':'completed'}]}
                        else:
                            response = {'model':'smoke-returned','content':[{'type':'tool_use','id':scenario.search_id,
                                'name':'web_search','input':{'message_id':scenario.message_id}}],'stop_reason':'tool_use'}
                    elif owner.protocol == 'openai_chat':
                        response = {'model':'smoke-returned','choices':[{'index':0,'message':{'role':'assistant','content':text},'finish_reason':'stop'}]}
                    elif owner.protocol == 'chatgpt_subscription':
                        response = {'id':'resp-smoke','object':'response','status':'completed','model':'smoke-returned','output':[
                            {'id':'msg-smoke','type':'message','role':'assistant','status':'completed','content':[{'type':'output_text','text':text}]}]}
                    else:
                        response = {'model':'smoke-returned','content':[{'type':'text','text':text}],'stop_reason':'end_turn'}
                    if owner.protocol == "openai_chat":
                        response.update(id="chatcmpl-smoke",object="chat.completion")
                    elif owner.protocol == "anthropic_messages":
                        response.update(id="msg-smoke",type="message",role="assistant")
                    if owner.protocol == 'chatgpt_subscription':
                        data = ('event: response.completed\ndata: '+json.dumps({'type':'response.completed','response':response},ensure_ascii=False)+'\n\n').encode()
                        if scenario.name == "normal":
                            delta = "STREAM_REASONING_MUST_NOT_APPEAR" + "x" * 4096
                            prefix = "".join("event: response.reasoning_summary_text.delta\ndata: " +
                                json.dumps({"type":"response.reasoning_summary_text.delta",
                                    "response_id":"resp-smoke", "item_id":"reasoning-smoke",
                                    "output_index":0, "summary_index":0, "sequence_number":index,
                                    "delta":delta}) + "\n\n" for index in range(128)).encode()
                            data = prefix + data
                            owner.large_stream_bytes = len(data)
                            assert 256 * 1024 < len(data) < 8 * 1024 * 1024
                    else: data = json.dumps(response,ensure_ascii=False).encode()
                    self.send_response(200)
                    self.send_header('Content-Type','text/event-stream' if owner.protocol == 'chatgpt_subscription' else 'application/json')
                    self.send_header('Content-Length',str(len(data)))
                    self.send_header("Connection", "close")
                    self.close_connection = True
                    self.end_headers()
                    try: self.wfile.write(data)
                    except (BrokenPipeError, ConnectionResetError):
                        assert scenario.kind == 'hold', 'unexpected disconnected response'
                    if scenario.kind != 'search' or follow_search: scenario.finished.set()
                except Exception as error:
                    with owner.lock: owner.failures.append(type(error).__name__ + ': ' + str(error))
                    if scenario: scenario.received.set()
                    try:
                        self.send_response(500)
                        self.send_header("Content-Length", "0")
                        self.send_header("Connection", "close")
                        self.close_connection = True
                        self.end_headers()
                    except (BrokenPipeError, ConnectionResetError): pass
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever,daemon=True)
        self.thread.start()
    @property
    def base(self): return 'http://127.0.0.1:' + str(self.server.server_port) + '/v1'
    def close(self):
        self.server.shutdown();self.server.server_close();self.thread.join(5)
