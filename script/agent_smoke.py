#!/usr/bin/env python3
"""Real isolated PTY + CLI + MCP + loopback HTTP; no live account or provider."""
import argparse
import codecs
import fcntl
import html
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time
import tomllib
import unicodedata
import urllib.parse
import urllib.request

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--protocol', choices=['openai_chat', 'anthropic_messages', 'chatgpt_subscription'], default='openai_chat')
p.add_argument('--test-binary', type=Path, help='cfg(test) lib executable; required only for subscription PTY')
a = p.parse_args()
if a.protocol == 'chatgpt_subscription' and not a.test_binary:
    p.error('subscription requires the isolated cfg(test) lib executable')
if sys.platform != 'darwin' or not Path('/usr/bin/sandbox-exec').is_file():
    p.error('macOS sandbox-exec is required: fail closed rather than permit external network')
binary = str(a.binary.resolve())
root = a.output.resolve()
root.mkdir(mode=0o700, parents=True)
home = root / 'home'
home.mkdir(mode=0o700)
instance = root / 'instance'
env = {'HOME':str(home), 'XDG_CONFIG_HOME':str(home/'config'),
       'XDG_DATA_HOME':str(home/'data'), 'TMPDIR':str(root),
       'PATH':'/usr/bin:/bin:/usr/sbin:/sbin', 'TERM':'xterm-256color', 'COLORTERM':'truecolor'}
profile = root / 'loopback-only.sb'
profile.write_text('(version 1)\n(allow default)\n(deny network*)\n(allow network-inbound (local ip "localhost:*"))\n(allow network-outbound (remote ip "localhost:*"))\n')
sandbox = ['/usr/bin/sandbox-exec','-f',str(profile)]
width, height = 120, 36
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', height, width, 0, 0))
private = root/'private-control'
private.mkdir(mode=0o700)
control_file = private/'control.json'
browser_channel = private/'browser.json'
fixture_file = private/'browser.fixtures.json'
product = None
os.set_blocking(master,False)
raw = bytearray()
transcript = []
mcp = None
config_pids = []

class Surface:
    def __init__(self):
        self.cells = [[(' ', '#ddd', '#151519') for _ in range(width)] for _ in range(height)]
        self.row = self.col = 0
        self.fg, self.bg = '#ddd', '#151519'
    def paint(self, data):
        palette = ['#151519', '#d54e53', '#b9ca4a', '#e7c547', '#7aa6da', '#c397d8', '#70c0b1', '#ddd', '#666', '#ff3334', '#9ec400', '#e7c547', '#7aa6da', '#b77ee0', '#54ced6', '#fff']
        parts = re.split(r'(\x1b\[[0-?]*[ -/]*[@-~])', data)
        for part in parts:
            if part.startswith('\x1b['):
                op = part[-1]
                body = part[2:-1]
                if body.startswith(('?', '>', '<')):
                    continue
                nums = [int(x or '0') for x in body.split(';')] if body else [0]
                n = nums[0] or 1
                if op in 'Hf':
                    self.row = min(height-1, max(0, n-1))
                    self.col = min(width-1, max(0, (nums[1] if len(nums)>1 else 1)-1))
                elif op == 'A': self.row = max(0, self.row-n)
                elif op == 'B': self.row = min(height-1, self.row+n)
                elif op == 'C': self.col = min(width-1, self.col+n)
                elif op == 'D': self.col = max(0, self.col-n)
                elif op == 'G': self.col = min(width-1, n-1)
                elif op == 'J' and nums[0] in (2, 3):
                    self.cells = [[(' ', self.fg, self.bg) for _ in range(width)] for _ in range(height)]
                elif op == 'K':
                    start, end = (0, width) if nums[0] == 2 else ((0, self.col+1) if nums[0] == 1 else (self.col, width))
                    for col in range(start, end): self.cells[self.row][col] = (' ', self.fg, self.bg)
                elif op == 'm':
                    i = 0
                    while i < len(nums):
                        v = nums[i]
                        if v == 0: self.fg, self.bg = '#ddd', '#151519'
                        elif v == 39: self.fg = '#ddd'
                        elif v == 49: self.bg = '#151519'
                        elif 30 <= v <= 37: self.fg = palette[v-30]
                        elif 90 <= v <= 97: self.fg = palette[v-90+8]
                        elif 40 <= v <= 47: self.bg = palette[v-40]
                        elif v in (38, 48) and i+4 < len(nums) and nums[i+1] == 2:
                            color = '#%02x%02x%02x' % tuple(nums[i+2:i+5])
                            if v == 38: self.fg = color
                            else: self.bg = color
                            i += 4
                        elif v in (38, 48) and i+2 < len(nums) and nums[i+1] == 5:
                            index = nums[i+2]
                            if index < 16: color = palette[index]
                            elif index >= 232: color = '#%02x%02x%02x' % ((8+(index-232)*10,)*3)
                            else:
                                k = index-16
                                ramp = [0,95,135,175,215,255]
                                color = '#%02x%02x%02x' % (ramp[k//36], ramp[k//6%6], ramp[k%6])
                            if v == 38: self.fg = color
                            else: self.bg = color
                            i += 2
                        i += 1
                continue
            for char in part:
                if char == '\r': self.col = 0
                elif char == '\n': self.row = min(height-1, self.row+1)
                elif ord(char) < 32: continue
                elif unicodedata.combining(char): continue
                else:
                    w = 2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1
                    if self.col >= width: self.col, self.row = 0, min(height-1, self.row+1)
                    self.cells[self.row][self.col] = (char, self.fg, self.bg)
                    if w == 2 and self.col+1 < width: self.cells[self.row][self.col+1] = ('', self.fg, self.bg)
                    self.col += w
    def save(self, name):
        lines = ["".join(f'<span style="display:inline-block;width:{2 if unicodedata.east_asian_width(c) in ("W", "F") else 1}ch;color:{fg};background:{bg}">{html.escape(c)}</span>' for c,fg,bg in row if c) for row in self.cells]
        (root / (name+".html")).write_text('<meta charset="utf-8"><style>body{background:#151519;margin:20px}pre{font:14px/20px Menlo,monospace;color:#ddd}span{vertical-align:top}</style><pre>'+ "\n".join(lines)+"</pre>")
        (root / (name+'.cells.json')).write_text(json.dumps(self.cells, ensure_ascii=False))


def write_all(data):
    # Nonblocking PTYs may accept only a prefix; never drop a paste terminator.
    view = memoryview(data)
    offset = 0
    deadline = time.monotonic() + 10
    while offset < len(view):
        try:
            offset += os.write(master, view[offset:])
        except BlockingIOError:
            pass
        if offset < len(view):
            if time.monotonic() >= deadline:
                raise TimeoutError(f"PTY input stalled after {offset}/{len(view)} bytes")
            pump(.02)

def pump(seconds=.25):
    until = time.monotonic()+seconds
    while time.monotonic() < until:
        if select.select([master], [], [], .03)[0]:
            try: chunk = os.read(master, 65536)
            except OSError: break
            if not chunk: break
            raw.extend(chunk)
            if b'\x1b[6n' in chunk: write_all(b'\x1b[1;1R')

def enter(text):
    write_all(text.encode()+b'\r')
    pump(.3)

def shift_enter():
    write_all(b"\x1b[13;2u");pump(.25)
    write_all(b"\x1b[13;2:3u");pump(.1)


def screen(name):
    pump(.3)
    surface = Surface()
    surface.paint(raw.decode('utf8', 'replace'))
    surface.save(name)
    return surface

def visible_text():
    surface = Surface()
    surface.paint(raw.decode("utf8", "replace"))
    return ["".join(c[0] for c in row) for row in surface.cells]


def assistant_badge_color():
    surface = Surface();surface.paint(raw.decode("utf8", "replace"))
    return next((fg for char, fg, _ in surface.cells[0] if char == "✦"), None)

def wait_badge(color):
    for _ in range(80):
        pump(.1)
        if assistant_badge_color() == color: return
    raise AssertionError(("assistant badge", color, assistant_badge_color(), visible_text()))




def resize(columns, rows, name):
    global width, height
    width, height = columns, rows
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
    os.kill(product.pid, signal.SIGWINCH)
    pump(.4)
    return screen(name)

def stopped_records():
    return [json.loads(line) for path in (instance / "sessions").glob("*/journal.jsonl")
            for line in path.read_text().splitlines() if json.loads(line)["kind"] == "assistantStopped"]


def choose(label):
    """Select a visible menu row using only Down and Enter on the real VT."""
    for _ in range(64):
        rows = visible_text()
        if any("› " in row and label in row for row in rows):
            write_all(b"\r");pump(.2)
            return
        write_all(b"\x1b[B");pump(.06)
    raise AssertionError(f"menu item not reachable: {label}; screen={visible_text()!r}")

def back(count=1):
    for _ in range(count):
        write_all(b"\x1b");pump(.08)


def close_panels():
    # Esc only unwinds owned overlays/selection; it never exits the product.
    back(8)


def execute_command(command):
    """Product commands use Ctrl-O; local synthetic controls use ordinary Enter."""
    close_panels()
    if command.startswith(("/event ", "/local ", "/session ")):
        write_all(b"\x15")  # Clear only this test's owned input.
        enter(command)
        return
    write_all(b"\x0f");pump(.15)
    write_all(command.encode());pump(.15)
    write_all(b"\r");pump(.25)


def palette_command(command):
    execute_command(command)

def automatic_preference(enabled):
    settings_file = instance / "assistant.json"
    execute_command("/ai auto " + ("on" if enabled else "off"))
    assert json.loads(settings_file.read_text())["automatic"] is enabled


def session_permission(enabled):
    if cli("status")["sending_enabled"] != enabled:
        execute_command("/ai auto " + ("on" if enabled else "off"))
    assert cli("status")["sending_enabled"] is enabled


def cli(op, args=None, ok=True):
    command = sandbox + [binary,"--instance",str(instance),"agent",op,json.dumps(args or {},ensure_ascii=False)]
    child = subprocess.Popen(command,env=env,cwd=root,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    deadline = time.monotonic()+38
    while True:
        try:
            stdout,stderr = child.communicate(timeout=.03)
            break
        except subprocess.TimeoutExpired:
            pump(.03)  # Never let a blocked PTY write starve the real TUI event loop.
            if time.monotonic() >= deadline:
                child.kill();child.communicate(timeout=4)
                raise subprocess.TimeoutExpired(command,38)
    run = subprocess.CompletedProcess(command,child.returncode,stdout,stderr)
    transcript.append({'via':'cli', 'op':op, 'args':args, 'code':run.returncode, 'stdout':run.stdout, 'stderr':run.stderr})
    if ok:
        assert run.returncode == 0, run.stderr
        return json.loads(run.stdout)
    assert run.returncode != 0
    return run.stderr

counter = 0

def rpc(method, params=None, connection=None):
    global counter
    counter += 1
    connection = connection or mcp
    connection.stdin.write(json.dumps({'jsonrpc':'2.0','id':counter,'method':method,'params':params or {}})+'\n')
    connection.stdin.flush()
    deadline = time.monotonic()+38
    while not select.select([connection.stdout],[],[],0)[0]:
        assert time.monotonic() < deadline,"MCP response timed out"
        pump(.03)
    value = json.loads(connection.stdout.readline())
    transcript.append({'via':'mcp', 'method':method, 'params':params, 'response':value})
    assert value.get('id') == counter and 'error' not in value, value
    return value['result']

def tool(op, args=None):
    result = rpc('tools/call', {'name':'danmu_'+op,'arguments':args or {}})
    assert not result.get('isError'), result
    return json.loads(result['content'][0]['text'])

def terminal_result(caller, request):
    for _ in range(60):
        result = cli('result', {'session':session,'caller':caller,'request_id':request})
        if result['state'] not in ['accepted','sending','awaiting_approval']: return result
        pump(.1)
    raise AssertionError('execution did not finish')

from functools import partial
from smoke_support import FAKE_KEY, FAILED_KEY, ModelDriver, Scenario
from smoke_support import private_json as read_private_json, private_write

fixture_secrets = {FAKE_KEY, FAILED_KEY, 'UPSTREAM_SECRET_SHOULD_NOT_APPEAR'}
private_json = partial(read_private_json, secrets=fixture_secrets)



def until(predicate, reason, timeout=20):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        assert product.poll() is None, 'owned TUI exited: ' + raw.decode('utf8','replace')[-2000:]
        assert not driver.failures, driver.failures
        value = predicate()
        if value: return value
        pump(.03)
    screen("failure")
    raise AssertionError(reason + "; screen=" + repr(visible_text()))


def records():
    out = []
    for path in root.rglob('journal.jsonl'):
        # Only newline-terminated records are committed during concurrent append.
        committed = path.read_bytes().rpartition(b"\n")[0]
        out.extend(json.loads(line) for line in committed.splitlines())
    return out


def completed(scenario):
    for record in records():
        value = record['payload']
        if isinstance(value,dict) and value.get('stage') == 'round_completed' and value.get('round_id') == scenario.round_id:
            return value
    return None


def model_result(scenario):
    return cli('result',{'session':session,'caller':'danmu-assistant','request_id':scenario.round_id+'-0'})


def dispatch(name, question, reply, kind='answer', previous=None):
    scenario = Scenario(name,reply,kind,previous)
    driver.pending.put(scenario)
    execute_command('/event 42 ' + question)
    until(scenario.received.is_set,'loopback did not receive '+name)
    if kind != 'hold': until(lambda:completed(scenario),'no journal terminal round '+name)
    return scenario


def set_field(label,value):
    choose(label)
    write_all(b'\x15' + value.encode() + b'\r');pump(.1)


def approve(scenario):
    execute_command('/review')
    until(lambda:any(scenario.reply in row for row in visible_text()),'candidate not visible')
    shift_enter()
    return until(lambda: (r if (r:=model_result(scenario))['state'] not in ['accepted','sending','awaiting_approval'] else None),
        'candidate delivery did not terminate')


def click_visible(label):
    surface = Surface();surface.paint(raw.decode('utf8','replace'))
    for y,row in enumerate(surface.cells):
        text = ''.join(c[0] for c in row)
        if label in text:
            x = text.index(label)+1
            write_all(f'\x1b[<0;{x+1};{y+1}M\x1b[<0;{x+1};{y+1}m'.encode());pump(.15)
            return
    raise AssertionError('mouse target not on real VT: '+label)



def complete_browser_login(expect_initial=False):
    channel = until(lambda:private_json(browser_channel) if browser_channel.exists() else None,'test browser channel missing')
    auth = urllib.parse.urlparse(channel['authorize_url'])
    assert auth.hostname == '127.0.0.1' and auth.path == '/authorize'
    query = urllib.parse.parse_qs(auth.query)
    assert query['client_id'] == ['dynamic_agent_client' if expect_initial else 'oaiapp_fixture']
    assert 'chatgpt.tokens.use.direct' in query['scope'][0].split()
    assert query['code_challenge_method'] == ['S256'] and query.get('code_challenge')
    redirect = urllib.parse.urlparse(query['redirect_uri'][0])
    assert redirect.hostname == '127.0.0.1' and redirect.path == '/auth/callback'
    callback = query['redirect_uri'][0]+'?'+urllib.parse.urlencode({'state':query['state'][0],
        'code':'fixture-code','client_id':'oaiapp_fixture'})
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(callback,timeout=5) as response: assert response.status == 200
    store_file = instance/'assistant-secrets'/'chatgpt.json'
    store = until(lambda: saved if store_file.exists() and
        (saved := private_json(store_file)).get("accounts") and saved["accounts"][0].get("tokens")
        else None,"login was not atomically persisted")
    return store

driver = ModelDriver(protocol=a.protocol, private_root=private, fixture_secrets=fixture_secrets)
if a.protocol == 'chatgpt_subscription':
    private_write(control_file,{'root':str(instance),'oauth_origin':driver.base.removesuffix('/v1'),
        'api_origin':driver.base.removesuffix('/v1'),'browser_channel':str(browser_channel)})
    env.update(DANMU_TEST_SUBSCRIPTION_PHASE='pty',DANMU_TEST_SUBSCRIPTION_CONTROL=str(control_file))
    command = [str(a.test_binary.resolve()),"--exact","terminal::agent::tests::subscription_pty_instance","--ignored","--nocapture"]
else: command = [binary,'--instance',str(instance),'local']
product = subprocess.Popen(sandbox+command,stdin=slave,stdout=slave,stderr=slave,env=env,cwd=root,start_new_session=True)
os.close(slave)
results = {'protocol':a.protocol,'isolation':'clean HOME/XDG, mandatory loopback-only sandbox'}
try:
    until(lambda:instance.joinpath('instance.json').exists(),'local endpoint did not start')
    status = cli('status');session = status['session']
    assert status['driver'] == 'external'
    mcp = subprocess.Popen(sandbox + [binary,'--instance',str(instance),'mcp'],stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,env=env,cwd=root)
    rpc('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'local-safety-probe','version':'1'}})
    mcp.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');mcp.stdin.flush()
    assert {t['name'] for t in rpc('tools/list')['tools']} == {
        'danmu_status','danmu_messages','danmu_report','danmu_reply','danmu_result'}
    assert tool('status')['session'] == session
    results['same_instance_cli_mcp'] = 'passed'
    configs = root/'host-configs';configs.mkdir()
    host_results = {}
    for host in ['omp','claude','codex','opencode','gemini','cursor','vscode']:
        generated = subprocess.check_output([sys.executable,str(Path(__file__).with_name('agent_config.py')),
            '--host',host,'--binary',binary,'--instance',str(instance)],text=True,env=env)
        (configs/(host+('.toml' if host == 'codex' else '.json'))).write_text(generated)
        if host == 'codex': config = tomllib.loads(generated)['mcp_servers']['shisui_danmu']
        elif host == 'opencode': config = json.loads(generated)['mcp']['shisui_danmu']
        elif host == 'vscode': config = json.loads(generated)['servers']['shisui_danmu']
        else: config = json.loads(generated)['mcpServers']['shisui_danmu']
        cmd = config['command'] if isinstance(config['command'],list) else [config['command']]+config['args']
        peer = subprocess.Popen(sandbox+cmd,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
            text=True,env=env,cwd=root)
        config_pids.append(peer.pid)
        try:
            rpc('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':host+'-config-probe','version':'1'}},peer)
            peer.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');peer.stdin.flush()
            assert len(rpc('tools/list',connection=peer)['tools']) == 5
            value = rpc('tools/call',{'name':'danmu_status','arguments':{}},peer)
            assert json.loads(value['content'][0]['text'])['session'] == session
            host_results[host] = 'generated_command_stdio_passed_native_host_not_run'
        finally:
            peer.stdin.close();peer.wait(timeout=5)
    results['seven_config_transport_probes'] = host_results

    write_all('甲🙂乙'.encode());pump(.1)
    write_all(b'\x1b[D\x1b[D');pump(.1)
    draft_before = screen('unicode-draft-before-key')
    draft_cursor = (draft_before.row,draft_before.col)
    execute_command('/settings')
    settings_view = screen('four-settings-categories')
    settings_text = '\n'.join(''.join(c[0] for c in row) for row in settings_view.cells)
    for label in ['B站账号与直播间','OBS','AI助手','系统与关于']: assert label in settings_text
    assert '外观与显示' not in settings_text
    choose('AI助手');choose('模型')
    model_view = screen('model-page-mouse-target')
    close_panels();execute_command('/settings');choose('AI助手')
    click_visible('模型');screen('mouse-open-model-page')
    choose('连接方式')
    if a.protocol == 'chatgpt_subscription':
        choose('ChatGPT订阅（官方 preview）')
        choose('Continue with ChatGPT')
        initial_store = complete_browser_login(expect_initial=True)
        until(lambda:json.loads((instance/'assistant.json').read_text())['api'].get('chatgpt_account'), 'selected account not saved')
        until(lambda: driver.catalog_count == 1, "login did not automatically read model catalog")
        until(lambda: any("SMOKE Model · smoke-model" in row for row in visible_text()),
            "login did not directly open model choices")
        assert not driver.requests, "login or catalog caused inference"
        assert json.loads((instance / "assistant.json").read_text())["api"]["model"] == "", "multiple models auto-selected"
        screen("subscription-login-automatically-opens-large-model-catalog")
        choose("SMOKE Model · smoke-model")
        settings_file = instance/'assistant.json'
        settings = json.loads(settings_file.read_text())
        assert settings['format']==4 and settings['api']['protocol']=='chatgpt_subscription'
        assert settings['api']['model']=='smoke-model' and settings['api']['base_url']=='https://api.openai.com/v1'
        assert not driver.requests, 'model catalog caused inference'
        close_panels()
        store_file = instance/'assistant-secrets'/'chatgpt.json'
        # Force only the synthetic local expiry stale; next real round must rotate it.
        expired = private_json(store_file);expired['accounts'][0]['tokens']['expires_at']=1
        expired['accounts'][0]['tokens']['earliest_refresh_at']=None;private_write(store_file,expired)
        results['official_subscription_browser_pkce_profile_catalog_selection'] = 'passed'
    else:
        choose('OpenAI兼容 Chat Completions' if a.protocol == 'openai_chat' else 'Anthropic Messages')
        set_field('API 基址',driver.base)
        set_field('模型名','smoke-model')
        settings_file = instance/'assistant.json'
        settings = json.loads(settings_file.read_text())
        assert settings['format'] == 4 and settings['api']['protocol'] == a.protocol
        assert settings['api']['base_url'] == driver.base and settings['api']['model'] == 'smoke-model'
        close_panels();execute_command('/ai key')
        write_all(b"\x1b[200~" + FAKE_KEY.encode() + b"\x1b[201~")
        until(lambda: "•" * len(FAKE_KEY) in "\n".join(visible_text()), "secret paste not rendered masked")
        masked = screen('key-paste-masked')
        assert FAKE_KEY not in raw.decode('utf8','replace')
        write_all(b'\r');pump(.1);close_panels()
        after_key = screen('unicode-draft-restored-after-key')
        assert any('甲🙂乙' in ''.join(c[0] for c in row) for row in after_key.cells)
        assert (after_key.row,after_key.col) == draft_cursor, 'secret save changed Unicode draft cursor'
        key_file = instance/'assistant-secrets'/'api-key.json'
        until(key_file.exists,'private key was not saved')
        assert key_file.stat().st_mode & 0o777 == 0o600
        assert key_file.parent.stat().st_mode & 0o777 == 0o700
        original_key = key_file.read_bytes()
        key_file.parent.chmod(0o500)
        try:
            execute_command('/ai key')
            write_all(b"\x1b[200~" + FAILED_KEY.encode() + b"\x1b[201~\r")
            until(lambda: "保存失败" in "\n".join(visible_text()) and "•" * len(FAILED_KEY) in "\n".join(visible_text()),
                "failed secret save did not retain masked input")
            screen('failed-key-save-still-hidden')
            assert key_file.read_bytes() == original_key
            assert FAILED_KEY not in raw.decode('utf8','replace')
        finally: key_file.parent.chmod(0o700)
        write_all(b'\x1b');pump(.1);close_panels()
        canceled = screen('failed-key-cancel-restores-draft')
        assert any('甲🙂乙' in ''.join(c[0] for c in row) for row in canceled.cells)
        assert (canceled.row,canceled.col) == draft_cursor
        assert not driver.requests, 'settings and key entry must not invoke model'
        results['four_categories_keyboard_mouse_hidden_key_failure_and_unicode_cursor'] = 'passed'
    execute_command("/ai model")
    until(lambda: "smoke-model" in "\n".join(visible_text()), "configured model page not visible")
    screen("configured-model-connection")
    close_panels()
    assert not driver.requests, "opening configured model page caused inference"
    write_all(b"\x15"); pump(.1)  # Only the owned fake human draft, never submit it.

    execute_command('/ai start')
    until(lambda:cli('status')['driver']=='runner','worker did not acquire driver')
    assert not driver.requests, 'start without messages invoked model'
    first = dispatch('normal','请回答 HTTP_FIRST_PUBLIC_QUESTION？','HTTP_NORMAL_REPLY')
    assert completed(first)['outcome'] == 'review'
    assert model_result(first)['state'] == 'awaiting_approval'
    screen('normal-model-candidate-before-approval')
    sent = approve(first)
    assert sent['state'] == 'confirmed' and sent['confirmed_segments'] and not sent['unconfirmed_segments']
    assert 'HTTP_NORMAL_REPLY' in ''.join(sent['confirmed_segments'])
    close_panels()
    follow = dispatch('followup','基于上一回答再说明 HTTP_FOLLOWUP_QUESTION？','HTTP_FOLLOWUP_REPLY',previous='HTTP_FIRST_PUBLIC_QUESTION')
    assert model_result(follow)['state'] == 'awaiting_approval'
    assert approve(follow)['state'] == 'confirmed'
    close_panels();screen('followup-context-and-actual-delivery')
    results['normal_http_review_confirmed_followup_local_memory'] = 'passed'
    if a.protocol == 'chatgpt_subscription':
        assert driver.large_stream_bytes > 256 * 1024
        assert "STREAM_REASONING_MUST_NOT_APPEAR" not in raw.decode("utf8", "replace")
        results["large_sse_stream_only_terminal_candidate_admitted"] = "passed"
        results["large_sse_stream_bytes"] = driver.large_stream_bytes
        assert driver.auth_calls.count('refresh_token') == 1
        rotated = private_json(store_file)
        assert rotated['accounts'][0]['tokens']['refresh_token'] != initial_store['accounts'][0]['tokens']['refresh_token']
        results['round_refresh_rotated_and_durably_saved'] = 'passed'

    bad = dispatch('bad_json','请回答 HTTP_BAD_JSON_QUESTION？','',kind='bad_json')
    assert completed(bad)['outcome'] == 'rejected'
    assert cli('status')['driver'] == 'runner'
    after_bad = dispatch('after_bad_json','继续回答 HTTP_AFTER_BAD_QUESTION？','HTTP_AFTER_BAD_REPLY')
    assert model_result(after_bad)['state'] == 'awaiting_approval'
    assert approve(after_bad)['state'] == 'confirmed'
    close_panels();screen('bad-json-rejected-next-question-works')
    results['candidate_parse_rejection_keeps_worker_running'] = 'passed'

    execute_command('/local uncertain')
    uncertain = dispatch('unknown_delivery','请回答 HTTP_UNCERTAIN_QUESTION？','HTTP_UNCERTAIN_REPLY')
    uncertain_result = approve(uncertain)
    assert uncertain_result["state"] == "uncertain"
    assert uncertain_result["unconfirmed_segments"] and not uncertain_result["confirmed_segments"]
    assert uncertain_result["duplicate_risk"] is True
    count_after_unknown = len(driver.requests)
    execute_command('/local confirmed')
    screen('unknown-delivery-no-retry')
    assert len(driver.requests) == count_after_unknown, 'unknown delivery triggered inference retry'
    results['unknown_delivery_no_automatic_repair_or_resend'] = 'passed'

    close_panels()
    held = dispatch('pause_inflight','请回答 HTTP_PAUSE_QUESTION？','HTTP_LATE_PAUSE_CANDIDATE',kind='hold')
    write_all('直播草稿🙂'.encode());pump(.1)
    write_all(b'\x10');pump(.1)
    stopped_round = until(lambda:completed(held),"pause did not terminate the local round")
    assert stopped_round["outcome"] in ["cancelled", "expired"]
    assert cli("status")["driver"] == "runner"  # Pause retains same-caller exclusion until stop.
    assert not cli('status')['sending_enabled']
    held.release.set();until(held.finished.is_set,'late upstream did not finish')
    paused = screen('pause-keeps-human-draft-ignores-late-result')
    assert any('直播草稿🙂' in ''.join(c[0] for c in row) for row in paused.cells)
    assert 'HTTP_LATE_PAUSE_CANDIDATE' not in json.dumps(records(),ensure_ascii=False)
    assert 'HTTP_LATE_PAUSE_CANDIDATE' not in raw.decode('utf8','replace')
    results['ctrl_p_cancels_inflight_keeps_draft_and_revokes_sending'] = 'passed'
    close_panels();write_all(b'\x15');pump(.1)

    execute_command('/ai start')
    until(lambda:cli('status')['driver']=='runner','restart after pause failed')
    scene_hold = dispatch('scene_change','请回答 HTTP_OLD_SCENE_QUESTION？','HTTP_LATE_OLD_SCENE_CANDIDATE',kind='hold')
    execute_command('/session end')
    execute_command('/session new')
    until(lambda:cli('status')['session'] != session,'new scene did not replace old session')
    session = cli('status')['session']
    assert not cli('status')['sending_enabled']
    scene_hold.release.set();until(scene_hold.finished.is_set,'old scene upstream did not finish')
    assert 'HTTP_LATE_OLD_SCENE_CANDIDATE' not in json.dumps(records(),ensure_ascii=False)
    assert 'HTTP_LATE_OLD_SCENE_CANDIDATE' not in raw.decode('utf8','replace')
    screen('new-session-ignores-old-model-result')
    results['scene_change_isolation'] = 'passed'

    # Search is deliberately unavailable: the sandbox forbids the real Exa origin.
    # The actual MCP client attempts it once; the model receives its explicit failure.
    execute_command('/pause')
    execute_command('/ai replies')
    choose('联网搜索')
    assert json.loads(settings_file.read_text())['web_search'] is True
    close_panels();execute_command('/ai start')
    until(lambda:cli('status')['driver']=='runner','search worker did not start')
    automatic_preference(True);session_permission(True)
    search = dispatch('search_unavailable','HTTP_SEARCH_PUBLIC_QUESTION 今日公开资料是什么？','HTTP_SEARCH_REVIEW_ONLY',kind='search')
    search_record = completed(search)
    assert search_record['search_state'] == 'unavailable'
    assert 'search_unavailable' in search_record['review_reason']
    assert model_result(search)['state'] == 'awaiting_approval', 'failed search automatically sent'
    assert sum(r['scenario']=='search_unavailable' for r in driver.requests) == 2, 'search exceeded one continuation'
    screen('search-unavailable-forces-review')
    results['single_search_failure_has_explicit_tool_result_and_forces_review'] = 'passed'

    protected = {'session':session,'caller':'danmu-assistant','request_id':'outsider-same-caller',
        'message_id':search.message_id,'text':'must_not_queue','candidate':True}
    assert 'runner_active' in cli('reply',protected,ok=False)
    assert 'runner_active' in cli('report',{'session':session,'caller':'danmu-assistant',
        'request_id':'outsider-report','message_id':search.message_id,'state':'processing'},ok=False)
    independent = dict(protected,caller='independent-probe',request_id='independent-review',text='INDEPENDENT_CANDIDATE')
    assert tool('reply',independent)['state'] == 'awaiting_approval'
    assert tool('status')['driver'] == 'runner'
    screen('external-driver-contract-preserved')
    results['runner_same_caller_exclusion_independent_external_review'] = 'passed'
    execute_command('/pause')
    execute_command("/diag"); write_all(b"\x1bOP")
    until(lambda: "unavailable" in "\n".join(visible_text()), "search diagnostics not visible")
    screen("model-protocol-returned-model-search-diagnostics")
    close_panels()
    screen("final-paused-local-workbench")
    assert not cli('status')['sending_enabled']
    assert not driver.failures
    for late in ["HTTP_LATE_PAUSE_CANDIDATE", "HTTP_LATE_OLD_SCENE_CANDIDATE"]:
        assert late not in json.dumps(records(),ensure_ascii=False)
        assert late not in raw.decode("utf8","replace")
    if a.protocol == 'chatgpt_subscription':
        execute_command('/ai start')
        until(lambda:cli('status')['driver']=='runner','quota probe start failed')
        quota = dispatch('quota_pause','HTTP_QUOTA_PUBLIC_QUESTION 请回答？','',kind='quota')
        assert completed(quota)["outcome"] == "error"
        until(lambda:not json.loads(settings_file.read_text())["resume_on_start"],
            "quota did not persist revocation")
        assert not cli('status')['sending_enabled']
        quota_settings = json.loads(settings_file.read_text())
        assert quota_settings["resume_on_start"] is False
        assert quota_settings["automatic"] is True  # Saved preference is not a live authorization.
        assert quota_settings["api"]["protocol"] == "chatgpt_subscription"
        assert private_json(store_file)['accounts'][0]['tokens'], 'quota destroyed valid credentials'
        until(lambda: any(row.lstrip().startswith("!") for row in visible_text()),
            "fatal HTTP notification was not shown")
        write_all("通知消退草稿甲🙂乙".encode())
        screen("fatal-notice-before-expiry")
        requests_before_expiry = len(driver.requests)
        until(lambda: not any(row.lstrip().startswith("!") for row in visible_text()),
            "fatal HTTP notification did not expire", timeout=18)
        expired_notice = screen("fatal-notice-expired-worker-stays-paused")
        assert any("通知消退草稿甲🙂乙" in row for row in visible_text())
        assert len(driver.requests) == requests_before_expiry, "expiry restarted inference"
        assert not cli("status")["sending_enabled"]
        assert not json.loads(settings_file.read_text())["resume_on_start"]
        assert completed(quota)["outcome"] == "error"
        results["fatal_notice_expires_without_resuming_or_clearing_error"] = "passed"
        execute_command("/diag")
        write_all(b"\x1bOP");pump(.15)
        quota_view = screen("quota-pauses-with-official-usage-guidance")
        quota_reason = completed(quota)["reason"]
        assert "subscription_sharing_usage_limit_exceeded" in quota_reason
        assert "https://chatgpt.com/settings/usage" in quota_reason
        assert 'UPSTREAM_SECRET_SHOULD_NOT_APPEAR' not in raw.decode('utf8','replace')
        results['quota_pauses_preserves_tokens_no_api_paid_fallback'] = 'passed'
        execute_command('/ai model')
        driver.revoke_status=503
        choose('退出当前账号')
        until(lambda:private_json(store_file)['accounts'][0].get('tokens') is None,'local signout did not clear tokens')
        until(lambda:'revoke' in driver.auth_calls,'bounded revocation not attempted')
        screen('subscription-local-signout-remote-revoke-failed')
        assert private_json(store_file)['accounts'][0]['id']==initial_store['accounts'][0]['id']
        # Reauthentication must reuse the same dynamic client/subject registration.
        until(lambda:any('重新登录当前账号' in row for row in visible_text()),'signout model page not restored')
        browser_channel.unlink()
        choose('重新登录当前账号')
        restored = complete_browser_login()
        assert restored['host_id']==initial_store['host_id']
        assert restored['accounts'][0]['id']==initial_store['accounts'][0]['id']
        assert restored['accounts'][0]['client_id']==initial_store['accounts'][0]['client_id']
        until(lambda: driver.catalog_count == 2, "reauthentication did not refresh model catalog")
        until(lambda: any("[当前] SMOKE Model · smoke-model" in row for row in visible_text()),
            "reauthentication did not retain valid selected model")
        assert json.loads(settings_file.read_text())["api"]["model"] == "smoke-model"
        screen("subscription-reauth-reuses-client-subject-binding")
        results['local_signout_failed_revocation_reauthentication_same_binding'] = 'passed'
        results['catalog_count'] = driver.catalog_count
        results['oauth_operations'] = driver.auth_calls
    results['request_count'] = len(driver.requests)
finally:
    driver.close()
    if mcp:
        mcp.stdin.close()
        try: mcp.wait(timeout=4)
        except subprocess.TimeoutExpired: mcp.terminate();mcp.wait(timeout=4)
    if product.poll() is None:
        write_all(b'\x03');pump(.1)
        try: product.wait(timeout=10)
        except subprocess.TimeoutExpired: product.terminate();product.wait(timeout=5)
    pump(.1);os.close(master)
    (root/'terminal.ansi').write_bytes(raw)
    (root/'protocol-transcript.json').write_text(json.dumps(transcript,ensure_ascii=False,indent=2))
    (root/'model-payloads.json').write_text(json.dumps(driver.requests,ensure_ascii=False,indent=2))
    results['model_failures'] = driver.failures
    results['product_exit_code'] = product.returncode
    results['owned_pids'] = {'product':product.pid,'mcp':mcp.pid if mcp else None,'config_probes':config_pids}
    results['endpoint_removed'] = not (instance/'instance.json').exists()
    results['processes_reaped'] = product.poll() is not None and (mcp is None or mcp.poll() is not None)
    evidence = [root/'terminal.ansi',root/'protocol-transcript.json',root/'model-payloads.json']
    evidence.extend(root.glob('*.html'));evidence.extend(root.glob('*.cells.json'))
    evidence.extend(root.rglob('journal.jsonl'))
    for path in evidence:
        assert not any(secret.encode() in path.read_bytes() for secret in fixture_secrets), 'fake credential leaked into outward evidence'
    results['credential_leak_scan'] = 'passed'
    (root/'results.json').write_text(json.dumps(results,ensure_ascii=False,indent=2))
    print(json.dumps(results,ensure_ascii=False,indent=2))
