#!/usr/bin/python3
"""Synthetic ACP search peer. No model, network, user files, or external tools.
Copied into a private fixture directory; identities only exercise protocol policy.
"""
import json
import pathlib
import sys

root = pathlib.Path(__file__).parent
config = json.loads((root / "search-fixture.json").read_text())
host = config["host"]
scenario = config["scenario"]
session = "search-fixture"
pending = None


def send(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def result(request, value):
    send({"jsonrpc": "2.0", "id": request["id"], "result": value})

def update(update_type, **fields):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {
        "sessionId": session, "update": {"sessionUpdate": update_type, **fields}}})


def text(value, identity):
    fields = {"messageId": identity} if host == "omp" else {}
    update("agent_message_chunk", content={"type": "text", "text": value}, **fields)


def search(request):
    global pending
    payload = json.loads(request["params"]["prompt"][0]["text"])
    round_id = payload["round_id"]
    call_id = "reused" if scenario == "cross_round" else "search-" + round_id
    before_id = "before-" + round_id
    body = json.dumps({"round_id": round_id, "candidates": [{
        "message_id": "viewer", "text": "暂无可核实的实时资料"}]}, ensure_ascii=False)
    fields = {"toolCallId": call_id, "locations": [], "content": []}
    if host == "omp":
        fields.update(kind="fetch", status="pending", title="核实公开天气",
                      rawInput={"query": "杭州 今日天气", "recency": "day"})
    else:
        fields.update(kind="search", status="in_progress",
                      title='Searching the web for: "杭州 今日天气"')
    if scenario == "permission":
        pending = request
        send({"jsonrpc": "2.0", "id": "search-permission", "method": "session/request_permission",
              "params": {"sessionId": session, "toolCall": fields,
                         "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]}})
        return
    text("先核实实时资料，不作为候选。", before_id)
    if scenario == "plain_messages":
        text(body, "after-" + round_id)
        result(request, {"stopReason": "end_turn"})
        return
    if scenario == "unknown_update":
        update("tool_call_update", toolCallId="unadmitted", status="completed")
    else:
        if scenario == "other_tool":
            fields.update(kind="execute", title="run_shell_command")
        elif scenario == "spoof_kind":
            if host == "omp":
                fields["rawInput"] = {"query": "天气", "path": "/private/file"}
            else:
                fields["title"] = "grep_search: weather"
        elif scenario == "file_location":
            fields["locations"] = [{"path": "/private/file"}]
        update("tool_call", **fields)
        if scenario == "unfinished":
            result(request, {"stopReason": "end_turn"})
            return
        if scenario == "premature_text":
            text(body, "after-" + round_id)
        terminal = {"toolCallId": call_id, "status": "failed" if scenario == "failed" else "completed"}
        if scenario == "identity_change":
            terminal["kind"] = "execute"
        elif scenario == "input_change":
            terminal["rawInput"] = {"query": "其他查询"}
        elif scenario == "file_result":
            terminal["content"] = [{"type": "diff", "path": "/private/file", "newText": "changed"}]
        elif scenario == "completed_error":
            terminal["rawOutput"] = {"details": {"error": "synthetic unavailable"}}
        update("tool_call_update", **terminal)
        if scenario == "repeated_terminal":
            update("tool_call_update", **terminal)
    text(body, before_id if scenario == "reused_message" else "after-" + round_id)
    result(request, {"stopReason": "end_turn"})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        result(request, {"protocolVersion": 1, "agentInfo": {
            "name": "oh-my-pi" if host == "omp" else "gemini-cli",
            "version": "18.1.15" if host == "omp" else "0.35.3"},
            "agentCapabilities": {"sessionCapabilities": {"close": {}}}, "authMethods": []})
    elif method == "session/new":
        assert request["params"].get("mcpServers", []) == []
        result(request, {"sessionId": session, "configOptions": []})
    elif method == "session/prompt":
        search(request)
    elif method == "session/close":
        result(request, {})
    elif request.get("id") == "search-permission" and "result" in request:
        outcome = request["result"]["outcome"]["outcome"]
        (root / "permission-outcome").write_text(outcome)
        if pending:
            result(pending, {"stopReason": "cancelled"})
            pending = None
