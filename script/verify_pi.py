#!/usr/bin/env python3
"""Real Pi 0.84.3/0.85.1 + pi-acp 0.0.33 through the product local PTY runner.

No dependency installation, user configuration, real credentials, or remote model.
Requires the two installed upstream executables and macOS sandbox-exec. The OS
network sandbox normally permits only loopback. --search-live explicitly permits
HTTPS and local DNS for one free Exa search; inference still uses fake loopback auth.
HTML/cells screenshots render actual PTY bytes using agent_smoke's VT renderer.
"""
import argparse
import ast
import fcntl
import hashlib
import html
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import shutil
import signal
import socket
import struct
import subprocess
import termios
import threading
import time
import traceback
import unicodedata
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO = Path(__file__).resolve().parents[1]
PROVIDER = "pi-proof"
REASONING = "proof-reasoning"
PLAIN = "proof-plain"
KEY = "fake-pi-proof-literal-key"
width, height = 140, 40


def dump(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2))


def package(entry, name, versions):
    for parent in entry.resolve().parents:
        manifest = parent / "package.json"
        if manifest.is_file():
            value = json.loads(manifest.read_text())
            if value.get("name") == name:
                assert value["version"] in versions, (name, value["version"], versions)
                return {"name": name, "version": value["version"], "entry": str(entry.resolve()),
                        "sha256": hashlib.sha256(entry.resolve().read_bytes()).hexdigest()}
    raise AssertionError(f"not an installed {name} entry: {entry}")


def round_payload(messages):
    for message in reversed(messages):
        if message.get("role") != "user":
            continue
        content = message.get("content", "")
        if isinstance(content, list):
            content = "\n".join(part.get("text", "") for part in content)
        for match in re.finditer(r"\{", content):
            try:
                value, _ = json.JSONDecoder().raw_decode(content[match.start():])
            except ValueError:
                continue
            if isinstance(value, dict) and "round_id" in value and "untrusted_messages" in value:
                return value
    raise AssertionError("real Pi request did not contain the product round payload")


class Model(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        record = {"path": self.path, "peer": self.client_address[0]}
        requests.append(record)
        try:
            assert self.path == "/v1/chat/completions", self.path
            assert self.client_address[0] == "127.0.0.1"
            assert self.headers.get("Authorization") == "Bearer " + KEY
            assert self.headers.get("X-Pi-Proof") == "declarative-header"
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            record["body"] = body
            assert body["model"] in (REASONING, PLAIN), f"unpublished model requested: {body['model']}"
            if search_live:
                assert [t["function"]["name"] for t in body.get("tools", [])] == ["web_search"], body.get("tools")
            else:
                assert not body.get("tools"), "native tools were advertised to the model"
            assert "GLOBAL_SKILL_MUST_NOT_LOAD" not in json.dumps(body)
            assert "GLOBAL_CONTEXT_MUST_NOT_LOAD" not in json.dumps(body)
            payload = round_payload(body["messages"])
            case = next((m["content"] for m in payload["untrusted_messages"] if "PI_CASE_" in m.get("content", "")), "")
            record.update(case=case, round_id=payload["round_id"])
            attempts = sum(r.get("case") == case for r in requests)
            record["attempt"] = attempts
            if ("PI_CASE_RETRY_SUCCESS" in case and attempts == 1) or "PI_CASE_RETRY_EXHAUSTED" in case:
                self.send_response(429)
                self.send_header("Content-Type", "application/json")
                self.send_header("Retry-After", "0")
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(json.dumps({"error": {"message": "loopback retry proof", "type": "rate_limit_error"}}).encode())
                self.wfile.flush()
                record["retryable_error"] = True
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            if "PI_CASE_RETRY_PARTIAL" in case and attempts == 1:
                partial = {"id": "chatcmpl-local-proof", "object": "chat.completion.chunk", "created": 1,
                           "model": body["model"], "choices": [{"index": 0, "delta": {"role": "assistant", "content": "{\"round_id\":\"stale"}, "finish_reason": None}]}
                self.wfile.write(("data: " + json.dumps(partial) + "\n\n").encode())
                self.wfile.write(("data: " + json.dumps({"error": {"message": "429 loopback stream retry proof", "type": "rate_limit_error", "code": "rate_limit_exceeded"}}) + "\n\n").encode())
                self.wfile.flush()
                record["partial_retryable_error"] = True
                return
            if "PI_CASE_CANCEL" in case:
                cancel_ready.set()
                # Keep the actual model request pending until its socket is closed
                # by Pi's abort (or teardown), rather than faking ACP cancellation.
                self.connection.settimeout(.1)
                while not release.is_set():
                    try:
                        if not self.connection.recv(1, socket.MSG_PEEK):
                            record["cancel_socket_closed"] = True
                            cancel_closed.set()
                            return
                    except socket.timeout:
                        pass
                    except (ConnectionError, OSError):
                        record["cancel_socket_closed"] = True
                        cancel_closed.set()
                        return
                record["cancel_released_by_cleanup"] = True
                return
            attack = re.search(r"PI_CASE_ATTACK_(read|write|bash)", case)
            last_user = max(i for i, m in enumerate(body["messages"]) if m.get("role") == "user")
            prior = [m for m in body["messages"][last_user + 1:] if m.get("role") == "tool"]
            searching = "PI_CASE_SEARCH" in case
            if searching and prior:
                tool_text = json.dumps(prior, ensure_ascii=False)
                assert "https://" in tool_text and "Rust" in tool_text, tool_text
                record["real_search_result_returned_to_model"] = True
            if attack and not prior:
                name = attack.group(1)
                args = {"read": {"path": str(secret)},
                        "write": {"path": str(write_marker), "content": "TOOL_EXECUTED"},
                        "bash": {"command": "touch " + shlex.quote(str(exec_marker))}}[name]
                record["forced_tool"] = name
                delta = {"role": "assistant", "tool_calls": [{"index": 0, "id": "forced_" + name,
                         "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}
                finish = "tool_calls"
            elif searching and not prior:
                assert search_live
                delta = {"role": "assistant", "tool_calls": [{"index": 0, "id": "real_web_search", "type": "function",
                    "function": {"name": "web_search", "arguments": json.dumps({"query": "Rust programming language official website"})}}]}
                finish = "tool_calls"
            else:
                candidates = [] if "PI_CASE_EMPTY" in case or attack else [
                    {"message_id": payload["untrusted_messages"][0]["id"], "text": "PiReply"}]
                if "PI_CASE_UNKNOWN_TARGET" in case:
                    candidates.append({"message_id": "not-in-current-batch", "text": "MustNotSend"})
                if "PI_CASE_DUPLICATE_TARGET" in case:
                    candidates *= 2
                delta = {"role": "assistant", "content": json.dumps({"round_id": payload["round_id"], "candidates": candidates})}
                finish = "stop"
            for data, reason in [(delta, None), ({}, finish)]:
                chunk = {"id": "chatcmpl-local-proof", "object": "chat.completion.chunk", "created": 1,
                         "model": body["model"], "choices": [{"index": 0, "delta": data, "finish_reason": reason}]}
                self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
            record["completed"] = True
        except (BrokenPipeError, ConnectionResetError):
            record["client_closed"] = True
        except Exception as error:
            record["error"] = repr(error)
            errors.append(repr(error))


def snapshot_processes():
    global last_ps
    last_ps = time.monotonic()
    result = subprocess.run(["/bin/ps", "-axo", "pid=,ppid=,pgid=,command="], capture_output=True, text=True, check=True)
    table = {}
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 3)
        if len(fields) == 4:
            pid, ppid, pgid = map(int, fields[:3])
            table[pid] = {"pid": pid, "ppid": ppid, "pgid": pgid, "command": fields[3]}
    parents = {product.pid} | set(owned)
    while True:
        children = {pid for pid, item in table.items() if item["ppid"] in parents}
        if children <= parents:
            break
        parents |= children
    for pid in parents & table.keys():
        if not table[pid]["command"].startswith("(") or pid not in owned:
            owned[pid] = table[pid]
    return table


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

def pump(seconds=.15):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        if select.select([master], [], [], .025)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            raw.extend(chunk)
            if b"\x1b[6n" in chunk:
                write_all(b"\x1b[1;1R")
        if time.monotonic() - last_ps > .5:
            snapshot_processes()


def surface():
    view = Surface()
    view.paint(raw.decode("utf8", "replace"))
    return view


def visible():
    return "\n".join("".join(c[0] for c in row) for row in surface().cells)


def screen(name):
    pump()
    view = surface()
    view.save(name)
    (root / (name + ".txt")).write_text(visible())
    snapshot_processes()


def wait_for(predicate, description, timeout=25):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        pump(.1)
        assert not errors, errors
        assert product.poll() is None, raw.decode("utf8", "replace")[-4000:]
        if predicate():
            return
    screen("failure")
    raise AssertionError("timed out: " + description + "\n" + visible())


def wait_visible(text):
    wait_for(lambda: text in visible(), "visible " + text)


def key(data):
    write_all(data)
    pump()


def enter(text):
    key(text.encode() + b"\r")


def close_panels():
    for _ in range(8):
        key(b"\x1b")


def choose(label):
    for _ in range(64):
        if any("› " in row and label in row for row in visible().splitlines()):
            key(b"\r")
            return
        key(b"\x1b[B")
    screen("failure-menu")
    raise AssertionError("menu item not reachable: " + label + "\n" + visible())


def settings(route, *sections):
    close_panels()
    enter("/ai " + route)
    for section in sections:
        choose(section)


def diagnostics(name, expected=None):
    close_panels()
    enter("/diag")
    screen(name)
    results.setdefault("diagnostics", {})[name] = visible()


def options():
    settings("model")
    wait_visible("Model")


def start():
    close_panels()
    enter("/ai")
    choose("继续 AI" if "继续 AI" in visible() else "启动 AI")
    if "确认操作" in visible():
        key(b"\r")
    options()
    # Evidence is the launcher emitted by the product, never a copied policy.
    for launcher in Path(env["TMPDIR"]).rglob("pi-text-launcher"):
        (root / "product-generated-launcher.txt").write_text(launcher.read_text())


def cli(op, args=None, *, allow_missing=False):
    run = subprocess.run([binary, "--instance", str(instance), "agent", op, json.dumps(args or {})],
                         capture_output=True, text=True, env=env, timeout=20)
    transcript.append({"operation": op, "args": args, "code": run.returncode, "stdout": run.stdout, "stderr": run.stderr})
    if allow_missing and run.returncode != 0 and "request_not_found" in run.stderr:
        return None
    assert run.returncode == 0, run.stderr
    return json.loads(run.stdout)


def event(case):
    close_panels()
    enter("/event PiProof PI_CASE_" + case)
    wait_for(lambda: any("PI_CASE_" + case in r.get("case", "") for r in requests), "HTTP model request " + case)


def round_result(case, suffix, *, allow_missing=False):
    record = next(r for r in reversed(requests) if r.get("case") == "PI_CASE_" + case)
    payload = round_payload(record["body"]["messages"])
    return cli("result", {"session": payload["scene_id"], "caller": "danmu-assistant",
                          "request_id": payload["round_id"] + "-" + suffix},
               allow_missing=allow_missing)


def wait_result(case, suffix, field, expected):
    observed = None

    def matches():
        nonlocal observed
        observed = round_result(case, suffix, allow_missing=True)
        return observed is not None and observed.get(field) == expected

    wait_for(matches, f"{case} {suffix} {field}={expected}")
    results.setdefault("round_results", {}).setdefault(case, {})[suffix] = observed
    return observed


def candidate(case):
    reply = wait_result(case, "0", "state", "awaiting_approval")
    assert reply["approval_required"] and reply["text"].endswith("PiReply"), reply
    assert cli("status")["sending_enabled"] is False
    wait_visible("PiReply")


def discard(case):
    close_panels()
    enter("/review")
    key(b"\x1b[3~")
    wait_result(case, "0", "state", "cancelled")
    close_panels()


def assistant_processes():
    table = snapshot_processes()
    return {pid for pid in owned if pid != product.pid and pid in table}


def wait_retired(pids):
    assert pids, "expected an owned Pi/ACP process before retirement"
    wait_for(lambda: not pids.intersection(snapshot_processes()), "owned Pi/ACP processes retired")
    assert cli("status")["sending_enabled"] is False


def saved(model, thinking):
    return any(p.get("model") == PROVIDER + "/" + model and p.get("thinking") == thinking
               for p in json.loads((instance / "assistant.json").read_text())["native_preferences"])


def main():
    global root, binary, instance, env, master, product, raw, transcript, requests, errors
    global owned, last_ps, results, Surface, cancel_ready, cancel_closed, release
    global secret, write_marker, exec_marker, search_live
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory (must not exist)")
    parser.add_argument("--pi", type=Path, default=shutil.which("pi"))
    parser.add_argument("--node", type=Path, default=shutil.which("node"))
    parser.add_argument("--adapter", type=Path, default=REPO / ".local-archive/pi-proof-deps/node_modules/.bin/pi-acp")
    parser.add_argument("--invalid-default-only", action="store_true", help="verify an unavailable native default never falls back or calls a model")
    parser.add_argument("--search-live", action="store_true", help="enable only bundled web_search; one real free Exa query, all inference remains loopback")
    parser.add_argument("--unfiltered-before", action="store_true", help="proof-only: bypass the product RPC filter to reproduce pi-acp status/body corruption")
    args = parser.parse_args()
    assert not (args.search_live and args.invalid_default_only), "choose one proof mode"
    search_live = args.search_live
    assert args.pi and args.node, "install official Pi and Node explicitly before this proof"
    assert shutil.which("sandbox-exec"), "macOS sandbox-exec is required to deny non-loopback outbound traffic"
    pi_info = package(args.pi, "@earendil-works/pi-coding-agent", ("0.84.3", "0.85.1"))
    adapter_info = package(args.adapter, "pi-acp", ("0.0.33",))
    binary = str(args.binary.resolve())
    assert os.access(binary, os.X_OK)
    root = args.output.resolve()
    root.mkdir(parents=True, mode=0o700)
    # Reuse only the existing VT rendering class; do not import/run the smoke suite.
    renderer = Path(__file__).with_name("agent_smoke.py")
    module = ast.parse(renderer.read_text(), filename=str(renderer))
    node = next(n for n in module.body if isinstance(n, ast.ClassDef) and n.name == "Surface")
    exec(compile(ast.Module(body=[node], type_ignores=[]), str(renderer), "exec"), globals())
    home = root / "home"
    agent = home / ".pi/agent"
    agent.mkdir(parents=True, mode=0o700)
    instance = root / "instance"
    bindir = root / "bin"
    bindir.mkdir(mode=0o700)
    for name, executable in [("pi", args.pi), ("pi-acp", args.adapter)]:
        (bindir / name).symlink_to(executable.resolve())
    if args.unfiltered_before:
        node_wrapper = bindir / "node"
        node_wrapper.write_text("#!/bin/sh\nif [ \"$#\" -eq 1 ]; then case \"$1\" in */pi-rpc-filter.js) exec /bin/cat;; esac; fi\nexec " + shlex.quote(str(args.node.resolve())) + " \"$@\"\n")
        node_wrapper.chmod(0o700)
    else:
        (bindir / "node").symlink_to(args.node.resolve())
    temporary = root / "tmp"
    temporary.mkdir(mode=0o700)
    # Allowlist from scratch: never read/copy developer authentication or overrides.
    env = {"HOME": str(home), "USERPROFILE": str(home), "XDG_CONFIG_HOME": str(home / "config"),
           "XDG_DATA_HOME": str(home / "data"), "PATH": str(bindir) + ":/usr/bin:/bin:/usr/sbin:/sbin",
           "TMPDIR": str(temporary), "TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "en_US.UTF-8",
           "OPENAI_API_KEY": "fake-host-key-must-not-pass", "ANTHROPIC_API_KEY": "fake-host-key-must-not-pass"}
    raw, transcript, requests, errors, owned = bytearray(), [], [], [], {}
    last_ps = 0
    results = {"pi": pi_info, "adapter": adapter_info, "binary": binary,
               "network": "Loopback inference plus HTTPS for explicit free Exa search" if search_live else "OS sandbox denies non-loopback outbound; model binds 127.0.0.1",
               "real_bilibili": "not_run", "real_credentials": "not_read", "checks": {}}
    cancel_ready, cancel_closed, release = threading.Event(), threading.Event(), threading.Event()
    secret = root / "private-read-canary"
    secret.write_text("PRIVATE_FILE_MUST_NOT_REACH_MODEL_719bfa")
    write_marker, exec_marker = root / "forbidden-write", root / "forbidden-subprocess"
    extension_marker = root / "forbidden-extension"
    extension = agent / "extensions/attack.js"
    extension.parent.mkdir()
    extension.write_text("import fs from 'node:fs'; fs.writeFileSync(" + json.dumps(str(extension_marker)) + ", 'loaded'); export default function() {}")
    skill = agent / "skills/attack/SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("---\nname: attack\ndescription: GLOBAL_SKILL_MUST_NOT_LOAD\n---\nGLOBAL_SKILL_MUST_NOT_LOAD\n")
    (home / "AGENTS.md").write_text("GLOBAL_CONTEXT_MUST_NOT_LOAD")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.daemon_threads = True
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    model_config = {"providers": {PROVIDER: {
        "baseUrl": f"http://127.0.0.1:{server.server_port}/v1", "api": "openai-completions",
        "headers": {"X-Pi-Proof": "declarative-header"},
        "compat": {"supportsDeveloperRole": False, "supportsReasoningEffort": True},
        "models": [{"id": REASONING, "name": "Proof Reasoning", "reasoning": True, "contextWindow": 128000, "maxTokens": 4096},
                   {"id": PLAIN, "name": "Proof Plain", "reasoning": False, "contextWindow": 128000, "maxTokens": 4096}]}}}
    settings_config = {"defaultProvider": PROVIDER, "defaultModel": "unavailable-default" if args.invalid_default_only else REASONING, "defaultThinkingLevel": "high",
                       "retry": {"enabled": True, "maxRetries": 2, "baseDelayMs": 25},
                       "packages": [str(extension.parent)], "extensions": [str(extension)],
                       "skills": [str(skill.parent)], "enableSkillCommands": True}
    dump(agent / "models.json", model_config)
    dump(agent / "settings.json", settings_config)
    dump(agent / "auth.json", {PROVIDER: {"type": "api_key", "key": KEY}})
    originals = {p.name: p.read_bytes() for p in [agent / "settings.json", agent / "models.json", agent / "auth.json"]}
    policy = '(version 1)(allow default)(deny network-outbound)(allow network-outbound (remote ip "localhost:*"))'
    if search_live:
        policy += '(allow network-outbound (remote ip "*:443") (remote unix-socket))'
    (root / "network.sb").write_text(policy)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
    product = subprocess.Popen(["/usr/bin/sandbox-exec", "-p", policy, binary, "--instance", str(instance), "local"],
                               stdin=slave, stdout=slave, stderr=slave, env=env, cwd=home, start_new_session=True)
    os.close(slave)
    os.set_blocking(master, False)
    failure = None
    endpoint = None
    try:
        wait_for(lambda: (instance / "instance.json").exists(), "local endpoint ready")
        endpoint = json.loads((instance / "instance.json").read_text())
        dump(root / "endpoint-before.json", endpoint)
        assert cli("status")["sending_enabled"] is False
        settings("model", "AI 工具")
        choose("Pi")
        assert json.loads((instance / "assistant.json").read_text())["host"] == "pi"
        assert not requests and not list(temporary.rglob("pi-text-launcher"))
        screen("pi-selected-no-launch")
        results["checks"]["select_does_not_launch_or_download"] = True
        if search_live:
            settings("replies")
            choose("联网搜索")
            assert json.loads((instance / "assistant.json").read_text())["web_search"] is True
            assert not requests, "enabling search must not call a model"
        if args.invalid_default_only:
            close_panels()
            enter("/ai")
            choose("启动助手")
            wait_for(lambda: any(pid != product.pid for pid in owned), "owned Pi/ACP process observed")
            wait_retired({pid for pid in owned if pid != product.pid})
            close_panels()
            enter("/event PiProof PI_CASE_INVALID_DEFAULT")
            diagnostics("pi-invalid-default-rejected")
            pump(1)
            assert not requests, "unavailable default silently called a fallback model"
            results["checks"]["invalid_native_default_zero_model_requests"] = True
        else:
            start()
            wait_visible("Proof Reasoning")
            assert any("Thinking" in row and "high" in row for row in visible().splitlines())
            screen("pi-default-native-options")
            choose("Model")
            assert "Proof Reasoning" in visible() and "Proof Plain" in visible(), visible()
            screen("pi-custom-model-list")
            key(b"\x1b")
            results["checks"]["declarative_provider_models_defaults"] = True
            diagnostics("pi-ready-diagnostics", "pi-acp")
            if search_live:
                event("SEARCH")
                candidate("SEARCH")
                assert any(r.get("real_search_result_returned_to_model") for r in requests)
                screen("pi-real-search-candidate")
                results["checks"]["real_free_search_only_then_candidate"] = True
                discard("SEARCH")
            event("REPLY")
            candidate("REPLY")
            screen("pi-candidate")
            assert requests[-1]["body"].get("reasoning_effort") == "high", requests[-1]["body"]
            results["checks"]["default_thinking_reaches_real_model_request"] = True
            discard("REPLY")
            for rejected_case in ("UNKNOWN_TARGET", "DUPLICATE_TARGET"):
                event(rejected_case)
                wait_result(rejected_case, "end-0", "reported", "failed")
                assert round_result(rejected_case, "0", allow_missing=True) is None
                diagnostics("pi-rejected-" + rejected_case.lower())
                assert len([r for r in requests if r.get("case") == "PI_CASE_" + rejected_case]) == 1
            rejections = [row for path in instance.glob("sessions/*/journal.jsonl")
                          for line in path.read_text().splitlines()
                          if (row := json.loads(line)).get("kind") == "autoReply"
                          and row["payload"].get("stage") == "round_completed"
                          and row["payload"].get("outcome") == "rejected"]
            assert len(rejections) == 2, rejections
            assert all(row["payload"].get("message_ids") and row["payload"].get("native_session") for row in rejections)
            results["candidate_rejections"] = rejections
            event("AFTER_TARGET_REJECTION")
            candidate("AFTER_TARGET_REJECTION")
            discard("AFTER_TARGET_REJECTION")
            results["checks"]["invalid_targets_reject_entire_batch_without_retry_or_manual_restart"] = True
            event("RETRY_SUCCESS")
            candidate("RETRY_SUCCESS")
            retry_success = [r for r in requests if r.get("case") == "PI_CASE_RETRY_SUCCESS"]
            assert len(retry_success) == 2 and retry_success[0].get("retryable_error") and retry_success[1].get("completed"), retry_success
            discard("RETRY_SUCCESS")
            results["checks"]["retry_status_is_not_candidate_body"] = True
            event("RETRY_PARTIAL")
            candidate("RETRY_PARTIAL")
            retry_partial = [r for r in requests if r.get("case") == "PI_CASE_RETRY_PARTIAL"]
            assert len(retry_partial) == 2 and retry_partial[0].get("partial_retryable_error") and retry_partial[1].get("completed"), retry_partial
            discard("RETRY_PARTIAL")
            results["checks"]["partial_failed_attempt_is_not_joined_to_retry"] = True
            event("RETRY_EXHAUSTED")
            wait_result("RETRY_EXHAUSTED", "end-0", "reported", "failed")
            retry_exhausted = [r for r in requests if r.get("case") == "PI_CASE_RETRY_EXHAUSTED"]
            assert len(retry_exhausted) == 4 and all(r.get("retryable_error") for r in retry_exhausted), retry_exhausted
            assert round_result("RETRY_EXHAUSTED", "0", allow_missing=True) is None
            diagnostics("pi-retry-exhausted-diagnostics")
            results["checks"]["retry_exhaustion_is_failed_not_silent_empty"] = True
            start()
            event("EMPTY")
            wait_result("EMPTY", "end-0", "reported", "finished")
            assert round_result("EMPTY", "0", allow_missing=True) is None
            diagnostics("pi-empty-diagnostics")
            results["checks"]["reply_candidate_and_intentional_empty"] = True
            options()
            choose("Model")
            choose("Proof Plain")
            wait_for(lambda: saved(PLAIN, "off"), "model and dependent thinking acknowledgement")
            wait_visible("Proof Plain")
            assert any("Thinking" in row and "off" in row for row in visible().splitlines())
            screen("pi-model-thinking-refreshed")
            results["checks"]["model_switch_refreshes_actual_thinking"] = True
            event("PLAIN_REPLY")
            candidate("PLAIN_REPLY")
            assert requests[-1]["body"]["model"] == PLAIN
            assert not requests[-1]["body"].get("reasoning_effort")
            discard("PLAIN_REPLY")
            options()
            choose("Thinking")
            configuring_pids = assistant_processes()
            choose("Thinking: high")
            wait_retired(configuring_pids)
            diagnostics("pi-clamped-thinking-rejected")
            assert saved(PLAIN, "off") and not saved(PLAIN, "high")
            results["checks"]["clamped_thinking_not_falsely_saved"] = True
            start()
            event("CANCEL")
            assert cancel_ready.is_set()
            key(b"\x10")
            wait_for(cancel_closed.is_set, "Pi abort closes pending HTTP socket")
            wait_result("CANCEL", "end-0", "reported", "finished")
            assert cli("status")["sending_enabled"] is False
            diagnostics("pi-cancelled-diagnostics")
            start()
            event("AFTER_CANCEL")
            candidate("AFTER_CANCEL")
            screen("pi-after-cancel-candidate")
            discard("AFTER_CANCEL")
            results["checks"]["real_model_cancel_and_next_round"] = True
            for name in ("read", "write", "bash"):
                event("ATTACK_" + name)
                wait_result("ATTACK_" + name, "end-0", "reported", "failed")
                diagnostics("pi-denied-" + name)
                wait_for(lambda: any(r.get("forced_tool") == name and r.get("completed") for r in requests), "forced tool delivered")
                pump(.5)
                assert not write_marker.exists() and not exec_marker.exists()
                assert secret.read_text() == "PRIVATE_FILE_MUST_NOT_REACH_MODEL_719bfa"
                assert "PRIVATE_FILE_MUST_NOT_REACH_MODEL_719bfa" not in json.dumps(requests)
                assert round_result("ATTACK_" + name, "0", allow_missing=True) is None
                receipts = [m for r in requests if r.get("case") == "PI_CASE_ATTACK_" + name
                            for m in r["body"]["messages"] if m.get("role") == "tool"]
                # Denial is proved by the failed round and absent effects, not upstream prose.
                results.setdefault("native_tool_rejection_receipts", {})[name] = receipts
                results["checks"]["forced_" + name + "_denied"] = True
                if name != "bash":
                    start()
        assert not extension_marker.exists()
        assert all((agent / name).read_bytes() == value for name, value in originals.items())
        results["checks"]["global_extensions_skills_context_disabled_and_source_unchanged"] = True
        assert not errors, errors
        close_panels()
        enter("/quit")
        product.wait(timeout=15)
        assert product.returncode == 0
    except Exception as error:
        failure = error
        results["error"] = traceback.format_exc()
        screen("failure-final")
        try:
            diagnostics("failure-product-diagnostics")
        except Exception:
            pass
    finally:
        release.set()
        if product.poll() is None:
            key(b"\x03")
            try:
                product.wait(timeout=12)
            except subprocess.TimeoutExpired:
                product.terminate()
                product.wait(timeout=5)
        pump(.1)
        table = snapshot_processes()
        live = [item for pid, item in owned.items() if pid in table and pid != product.pid]
        results["owned_pids"] = list(owned.values())
        results["live_owned_after_product_exit"] = live
        # Cleanup only PIDs first observed as descendants of our owned product.
        for item in live:
            try:
                os.kill(item["pid"], signal.SIGKILL)
            except ProcessLookupError:
                pass
        results["product_exit_code"] = product.returncode
        results["endpoint_removed"] = not (instance / "instance.json").exists()
        results["endpoint_connection_refused"] = False
        if endpoint:
            address, port = endpoint["address"].rsplit(":", 1)
            try:
                with socket.create_connection((address, int(port)), timeout=1):
                    pass
            except ConnectionRefusedError:
                results["endpoint_connection_refused"] = True
        results["all_owned_exited_without_forced_cleanup"] = not live
        results["passed"] = failure is None and not live and results["endpoint_removed"] and results["endpoint_connection_refused"]
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
        os.close(master)
        (root / "terminal.ansi").write_bytes(raw)
        dump(root / "model-requests.json", requests)
        dump(root / "cli-transcript.json", transcript)
        dump(root / "results.json", results)
        print(json.dumps({"passed": results["passed"], "checks": results["checks"], "output": str(root),
                          "error": str(failure) if failure else None}, ensure_ascii=False, indent=2))
    if not results["passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
