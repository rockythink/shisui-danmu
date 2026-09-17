#!/usr/bin/python3
"""Synthetic ACP peer, NOT OMP or model inference. Only private files and stdio.
The reported OMP identity is fixture data exercising the production identity gate.
Run only from a copied private fixture directory containing fixture.json.
"""
import json
import os
import pathlib
import subprocess
import sys
import time

root = pathlib.Path(__file__).parent
config = json.loads((root / "fixture.json").read_text())
mode = config.get("mode", "normal")
log = root / "wire.jsonl"
session = "fixture-session-0"
serial = 0
pending = None
model = "fixture/a"
thinking = "low"

def record(direction, value):
    with log.open("a") as f:
        f.write(json.dumps({"fixture": True, "pid": os.getpid(), "direction": direction, "value": value}) + "\n")

def send(value):
    record("out", value)
    data = (json.dumps(value, ensure_ascii=False) + "\n").encode()
    if mode == "split":
        for i in range(0, len(data), 7):
            sys.stdout.buffer.write(data[i:i+7]); sys.stdout.buffer.flush()
    else:
        sys.stdout.buffer.write(data); sys.stdout.buffer.flush()

def result(req, value):
    send({"jsonrpc": "2.0", "id": req["id"], "result": value})

def update(kind, **fields):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session, "update": {"sessionUpdate": kind, **fields}}})

def options():
    return [
        {"id":"model", "name":"Fixture model", "category":"model", "type":"select", "currentValue":model, "options":[{"group":"offline", "name":"Offline", "options":[{"value":"fixture/a", "name":"A"},{"value":"fixture/b", "name":"B"}]}]},
        {"id":"thinking", "name":"Thought", "category":"thought_level", "type":"select", "currentValue":thinking, "options":[{"value":v,"name":v} for v in (["low","high"] if model=="fixture/a" else ["low"])]},
        {"id":"unsafe_toggle", "name":"Native permission", "type":"boolean", "currentValue":False},
        {"id":"future_type", "name":"Unknown", "type":"future", "currentValue":0},
        {"id":"custom", "name":"Custom", "category":"_fixture", "type":"select", "currentValue":"x", "options":[{"value":"x","name":"X"}]},
    ]

def setup():
    modes = {"currentModeId":"default", "availableModes":[{"id":"default","name":"Default"}]}
    if mode == "missing": return {"modes":modes}
    return {"configOptions":[] if mode == "empty" else options(), "modes":modes}

def answer(req):
    mode = config["answer_modes"].pop(0) if config.get("answer_modes") else config.get("mode", "normal")
    if mode == "silent_turn":
        update("agent_thought_chunk", content={"type":"text", "text":"No reply needed"})
        result(req, {"stopReason":"end_turn"})
        return
    payload = json.loads(req["params"]["prompt"][0]["text"])
    ids = [m["id"] for m in payload["untrusted_messages"]]
    body = {"round_id":payload["round_id"], "candidates":[{"message_id":ids[0], "text":"受控候选"}]}
    if mode == "no_answer": body["candidates"] = []
    if mode == "wrong_round": body["round_id"] = "old-round"
    if mode == "wrong_message": body["candidates"][0]["message_id"] = "not-in-batch"
    if mode == "mixed_candidate": body["candidates"].append({"message_id":"previous-question", "text":"不能发送的旧目标"})
    if mode == "extra_field": body["sent"] = True
    if mode == "duplicate_candidate": body["candidates"] *= 2
    if mode == "tool": update("tool_call", toolCallId="fixture-tool", title="Forbidden", kind="execute", status="pending")
    if mode == "cross_session":
        send({"jsonrpc":"2.0", "method":"session/update", "params":{"sessionId":"foreign-session", "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":json.dumps(body)}}}})
    else:
        update("agent_thought_chunk", content={"type":"text", "text":"NOT CANDIDATE"})
        text = json.dumps(body, ensure_ascii=False)
        fields = {} if mode == "no_message_id" else {"messageId":"fixture-message-"+payload["round_id"]}
        update("agent_message_chunk", content={"type":"text", "text":text[:len(text)//2]}, **fields)
        update("agent_message_chunk", content={"type":"text", "text":text[len(text)//2:]}, **fields)
    if mode == "wrong_id":
        send({"jsonrpc":"2.0", "id":"wrong-id", "result":{"stopReason":"end_turn"}})
        return
    result(req, {"stopReason":"max_tokens" if mode == "incomplete" else "end_turn"})
    if mode == "duplicate_id": result(req, {"stopReason":"end_turn"})
    if mode == "late":
        body["candidates"][0]["text"]="迟到正文不得覆盖"
        update("agent_message_chunk", content={"type":"text", "text":json.dumps(body)})

record("started", {"argv":sys.argv, "cwd":os.getcwd()})
for line in sys.stdin:
    req = json.loads(line)
    record("in", req)
    method = req.get("method")
    if method == "initialize":
        if mode == "eof": break
        if mode == "long_line":
            sys.stdout.write("x" * (256*1024+1)); sys.stdout.flush(); time.sleep(4); continue
        if mode == "flood":
            for i in range(4200): send({"jsonrpc":"2.0", "method":"_fixture/noop", "params":{}})
            continue
        if mode == "timeout": continue
        identity = {"name":"gemini-cli" if mode == "missing" else "oh-my-pi", "version":"0.35.3" if mode == "missing" else "18.1.14"}
        result(req, {"protocolVersion":2 if mode == "wrong_protocol" else 1, "agentInfo":identity, "authMethods":[], "agentCapabilities":{"loadSession":True, "sessionCapabilities":{"close":{}, **({} if mode == "load" else {"resume":{}})}}})
    elif method == "session/new":
        serial += 1; session = "fixture-session-"+str(serial); model="fixture/a"; thinking="low"
        result(req, {"sessionId":session, **setup()})
    elif method in ("session/load", "session/resume"):
        session=req["params"]["sessionId"]
        if method == "session/load" or mode == "resume_replay": update("agent_message_chunk", content={"type":"text", "text":"historical replay is not a candidate"})
        result(req, setup())
    elif method == "session/set_config_option":
        time.sleep(config.get("config_delay", 0))
        if mode != "config_refusal":
            if req["params"]["configId"] == "model": model=req["params"]["value"]; thinking="low"
            if req["params"]["configId"] == "thinking": thinking=req["params"]["value"]
        update("config_option_update", configOptions=options())
        result(req, {"configOptions":options()})
    elif method == "session/set_mode":
        update("current_mode_update", currentModeId=req["params"]["modeId"]); result(req,{})
    elif method == "session/prompt":
        if mode == "prompt_eof": break
        if mode in ("cancel", "ignore_cancel", "permission", "slow"):
            pending=req
            if mode == "ignore_cancel":
                child=subprocess.Popen(["/bin/sleep", "60"])
                record("child", {"pid":child.pid})
            if mode == "permission":
                send({"jsonrpc":"2.0", "id":"permission", "method":"session/request_permission", "params":{"sessionId":session,"toolCall":{"toolCallId":"t","title":"Forbidden"},"options":[{"optionId":"yes","name":"Allow","kind":"allow_once"}]}})
            if mode == "slow": time.sleep(config.get("prompt_delay", .12)); answer(req); pending=None
        else: answer(req)
    elif method == "session/cancel":
        if pending and mode != "ignore_cancel": result(pending,{"stopReason":"cancelled"}); pending=None
    elif method == "session/close": result(req,{})
    elif req.get("id") == "permission" and "result" in req:
        assert req["result"]["outcome"]["outcome"] == "cancelled"
        if pending: result(pending,{"stopReason":"cancelled"}); pending=None
record("exited", {})
