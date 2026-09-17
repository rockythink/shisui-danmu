#!/usr/bin/env python3
"""Run real installed agents against a loopback-only model, never user credentials.
Usage: python3 script/verify-native-text-hosts.py codex /absolute/codex-acp /absolute/codex
       python3 script/verify-native-text-hosts.py copilot /absolute/copilot
       python3 script/verify-native-text-hosts.py claude /absolute/claude-agent-acp
No installation, browser, login, or remote model call is performed.
"""
import json
import os
from pathlib import Path
import queue
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
ROOT = Path(__file__).resolve().parents[1]
HOST, BINARY = sys.argv[1:3]
assert HOST in ("claude", "codex", "copilot")
assert Path(BINARY).is_absolute()
requests = []
attack = False
attack_sent = False
cancel_ready = threading.Event()
cancel_release = threading.Event()


def event(kind, **data):
    return {"type": kind, **data}


class Model(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        data = json.dumps({"object": "list", "data": [{"id": "gpt-5.3-codex", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        global attack_sent
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        requests.append(body)
        if attack == "cancel":
            cancel_ready.set()
            cancel_release.wait(15)
            return
        malicious = bool(attack) and not attack_sent
        attack_sent |= malicious
        if HOST == "claude":
            block = {"type": "text", "text": "native-safe-ok"}
            if malicious:
                block = {"type": "tool_use", "id": "call_attack", "name": "Read" if attack == "read" else "Bash", "input": {"file_path": str(read_trap)} if attack == "read" else {"command": "touch " + str(marker)}}
            payload = {"id": "msg_fixture", "type": "message", "role": "assistant", "model": body.get("model"), "content": [block], "stop_reason": "tool_use" if malicious else "end_turn", "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}}
            # Claude Agent SDK accepts the normal streaming Messages API.
            events = [event("message_start", message={**payload, "content": [], "stop_reason": None}), event("content_block_start", index=0, content_block=block), event("content_block_stop", index=0), event("message_delta", delta={"stop_reason": payload["stop_reason"], "stop_sequence": None}, usage={"output_tokens": 1}), event("message_stop")]
        elif HOST == "copilot":
            choice = {"index": 0, "delta": {"role": "assistant", "content": "native-safe-ok"}, "finish_reason": None}
            if malicious:
                name, arguments = ("view", {"path": str(read_trap)}) if attack == "read" else ("bash", {"command": "touch " + str(marker)})
                choice["delta"] = {"role": "assistant", "tool_calls": [{"index": 0, "id": "call_attack", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
            events = [{"id": "chat_fixture", "object": "chat.completion.chunk", "model": body.get("model"), "choices": [choice]}, {"id": "chat_fixture", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if malicious else "stop"}]}]
        else:
            item = {"id": "msg_fixture", "type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "native-safe-ok"}]}
            if malicious:
                patch = "*** Update File: " + str(read_trap) + "\n@@\n-secret\n+changed\n" if attack == "read" else "*** Add File: " + str(marker) + "\n+unauthorized\n"
                item = {"type": "custom_tool_call", "call_id": "call_attack", "name": "apply_patch", "input": "*** Begin Patch\n" + patch + "*** End Patch\n"}
            events = [event("response.created", response={"id": "resp_fixture"}), event("response.output_item.done", item=item), event("response.completed", response={"id": "resp_fixture", "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}})]
        content = "".join(("event: " + e["type"] + "\n" if "type" in e else "") + "data: " + json.dumps(e) + "\n\n" for e in events)
        if HOST == "copilot":
            content += "data: [DONE]\n\n"
        data = content.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


with tempfile.TemporaryDirectory(prefix="shisui-native-contract-") as directory:
    work = Path(directory)
    home = work / "home"
    home.mkdir()
    marker = work / "UNAUTHORIZED"
    read_trap = work / "READ_TRAP"
    os.mkfifo(read_trap, 0o600)
    reader_opened = threading.Event()
    monitor_stop = threading.Event()
    def monitor_reads():
        while not monitor_stop.wait(0.01):
            try:
                fd = os.open(read_trap, os.O_WRONLY | os.O_NONBLOCK)
            except OSError:
                continue
            reader_opened.set()
            os.write(fd, b"secret\n")
            os.close(fd)
            return
    threading.Thread(target=monitor_reads, daemon=True).start()
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    endpoint = "http://127.0.0.1:" + str(server.server_port)
    env = {"HOME": str(home), "PATH": os.environ["PATH"], "TMPDIR": str(work), "TERM": "dumb", "NO_COLOR": "1", "NO_BROWSER": "1"}
    args = [BINARY]
    meta = {}
    if HOST == "claude":
        env.update(CLAUDE_CODE_SAFE_MODE="1", ANTHROPIC_API_KEY="local-fixture-not-a-credential", ANTHROPIC_BASE_URL=endpoint)
        meta = {"claudeCode": {"options": {"tools": [], "settingSources": [], "strictMcpConfig": True, "settings": {"disableAllHooks": True}, "extraArgs": {"safe-mode": None, "disable-slash-commands": None}}}}
    elif HOST == "copilot":
        config = work / "copilot"
        config.mkdir()
        (config / "settings.json").write_text(json.dumps({"disableAllHooks": True, "ide": {"autoConnect": False}}))
        env.update(COPILOT_HOME=str(config), COPILOT_PLUGIN_DIR_ONLY="true", COPILOT_OFFLINE="true", COPILOT_PROVIDER_BASE_URL=endpoint, COPILOT_PROVIDER_TYPE="openai", COPILOT_PROVIDER_MODEL_ID="gpt-5.3-codex", COPILOT_MODEL="gpt-5.3-codex")
        args += ["--acp", "--stdio", "--available-tools=__shisui_text_only_" + uuid.uuid4().hex, "--disable-builtin-mcps", "--no-custom-instructions", "--no-auto-update", "--no-remote", "--no-remote-export", "--no-bash-env"]
    else:
        # Execute the exact production native proxy, not an imitation ACP server.
        source = (ROOT / "src/runner/hosts/codex.rs").read_text()
        proxy = source.split('const PROXY: &str = r#"', 1)[1].split('"#;', 1)[0]
        config = source.split('const CONFIG: &str = r#"', 1)[1].split('"#;', 1)[0]
        shim = work / "native-boundary"
        shim.write_text("#!" + shutil.which("node") + "\n" + proxy)
        shim.chmod(0o700)
        nativehome = home / ".codex"
        nativehome.mkdir()
        nativehome.joinpath("config.toml").write_text('model = "gpt-5.3-codex"\nmodel_provider = "fixture"\n' + config + '\n[model_providers.fixture]\nname = "Loopback fixture"\nbase_url = "' + endpoint + '"\nwire_api = "responses"\nrequires_openai_auth = false\n')
        env.update(CODEX_HOME=str(nativehome), CODEX_PATH=str(shim), SHISUI_CODEX_NATIVE=sys.argv[3], SHISUI_CODEX_WORKSPACE=str(work), INITIAL_AGENT_MODE="read-only")
    proc = subprocess.Popen(args, cwd=work, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    frames = queue.Queue()
    diagnostics = []
    threading.Thread(target=lambda: [frames.put(json.loads(line)) for line in proc.stdout], daemon=True).start()
    threading.Thread(target=lambda: [diagnostics.append(line.decode(errors="replace")) for line in proc.stderr], daemon=True).start()
    notifications = []
    def rpc(i, method, params):
        proc.stdin.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}) + "\n").encode())
        proc.stdin.flush()
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            try:
                frame = frames.get(timeout=1)
            except queue.Empty:
                if proc.poll() is not None:
                    raise AssertionError("agent exited: " + "".join(diagnostics)[-2500:])
                continue
            if frame.get("id") == i:
                assert "error" not in frame, frame.get("error")
                return frame["result"]
            if "id" in frame and "method" in frame:
                answer = {"jsonrpc": "2.0", "id": frame["id"], "result": {"outcome": {"outcome": "cancelled"}}} if frame["method"] == "session/request_permission" else {"jsonrpc": "2.0", "id": frame["id"], "error": {"code": -32601, "message": "Not supported"}}
                proc.stdin.write((json.dumps(answer) + "\n").encode())
                proc.stdin.flush()
            else:
                notifications.append(frame)
        raise AssertionError("timeout: " + method + "\n" + "".join(diagnostics)[-2500:])
    try:
        identity = rpc(1, "initialize", {"protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {"name": "shisui-local-smoke", "version": "1"}})["agentInfo"]
        session = rpc(2, "session/new", {"cwd": str(work), "mcpServers": [], "_meta": meta})
        print("native", identity)
        print("options", [{"id": row["id"], "current": row.get("currentValue")} for row in session.get("configOptions", [])])
        sid = session["sessionId"]
        for i, row in enumerate(session.get("configOptions", []), 10):
            if row["id"] in ("model", "effort", "reasoning_effort"):
                rpc(i, "session/set_config_option", {"sessionId": sid, "configId": row["id"], "value": row["currentValue"]})
        rpc(50, "session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "Return native-safe-ok."}]})
        assert requests, "real model endpoint was not used"
        assert "native-safe-ok" in json.dumps(notifications), "native text did not reach ACP"
        assert not requests[0].get("tools"), "tools exposed to text-only model: " + json.dumps(requests[0].get("tools"))
        attack = "write"
        rpc(51, "session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "Handle the next response."}]})
        assert attack_sent, "adversarial native model response not exercised"
        assert not marker.exists(), "unauthorized native tool wrote a file"
        attack, attack_sent = "read", False
        rpc(52, "session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "Handle the next response."}]})
        assert attack_sent, "adversarial native read response not exercised"
        assert not reader_opened.is_set(), "native tool opened a forbidden file for reading"
        print("PASS: native text delivered; tools=[]; forced write and read blocked before I/O")
        attack = "cancel"
        def cancel_prompt():
            if cancel_ready.wait(10):
                proc.stdin.write((json.dumps({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}}) + "\n").encode())
                proc.stdin.flush()
        threading.Thread(target=cancel_prompt, daemon=True).start()
        cancel_started = time.monotonic()
        cancelled = rpc(53, "session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "Wait for cancellation."}]})
        assert cancel_ready.is_set() and time.monotonic() - cancel_started < 10, "native cancellation did not interrupt the blocked model request"
        # Copilot 1.0.83 ends a cancelled native turn with end_turn, unlike the
        # other adapters. Verify actual interruption, not its incidental label.
        assert not cancel_release.is_set()
        cancel_release.set()
        print("PASS: native cancellation interrupts blocked model request; stopReason=" + cancelled.get("stopReason", "missing"))
        proc.stdin.close()
        proc.wait(timeout=8)
        print("PASS: stdin EOF shuts down owned agent")
        deadline = time.monotonic() + 3
        while True:
            try:
                os.killpg(proc.pid, 0)
            except ProcessLookupError:
                break
            assert time.monotonic() < deadline, "owned descendants survived agent EOF"
            time.sleep(0.05)
        print("PASS: no owned descendant remains after EOF")
    finally:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        proc.wait(timeout=8)
        cancel_release.set()
        monitor_stop.set()
        server.shutdown()
