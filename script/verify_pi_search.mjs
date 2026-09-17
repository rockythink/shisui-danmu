#!/usr/bin/env node
// Pure extension boundary checks; --live additionally uses one unauthenticated Exa search.
import assert from "node:assert/strict";
import install from "../src/runner/hosts/pi_search.js";

const nativeFetch = globalThis.fetch;
const checks = {};
function extension() {
  let tool;
  const hooks = new Map();
  install({ on: (event, handler) => hooks.set(event, handler), registerTool: (value) => { tool = value; }, setActiveTools() {} });
  return { execute: (params, signal) => tool.execute("proof", params, signal), next: () => hooks.get("before_agent_start")() };
}
function sse(id, result) {
  return new Response(`: keepalive\r\n\r\nevent: message\r\ndata: ${JSON.stringify({ jsonrpc: "2.0", id, result })}\r\n\r\n`, {
    headers: { "content-type": "text/event-stream", "mcp-session-id": "proof-session" },
  });
}
try {
  let searches = 0;
  globalThis.fetch = async (_url, options) => {
    if (options.method === "DELETE") return new Response(null, { status: 204 });
    const request = JSON.parse(options.body);
    if (request.method === "initialize") return sse(request.id, { protocolVersion: "2025-03-26" });
    if (request.method === "notifications/initialized") return new Response(null, { status: 202 });
    searches++;
    return sse(request.id, { content: [{ type: "text", text: "Title: 中文资料\nURL: https://example.org/\n" + "长文".repeat(4000) }] });
  };
  const scoped = extension();
  const first = await scoped.execute({ query: "公开资料" });
  assert.match(first.content[0].text, /https:\/\/example.org/);
  assert.ok(first.content[0].text.length <= 4100);
  await assert.rejects(scoped.execute({ query: "第二个问题" }));
  assert.equal(searches, 1);
  scoped.next();
  await scoped.execute({ query: "下一轮问题" });
  assert.equal(searches, 2);
  checks.one_search_per_prompt_and_bounded_untrusted_results = true;

  let outbound = 0;
  globalThis.fetch = async () => { outbound++; throw new Error("must not leave process"); };
  for (const params of [{ query: "", }, { query: "line\nbreak" }, { query: "x".repeat(241) }, { query: "q", url: "http://127.0.0.1/" }]) {
    await assert.rejects(extension().execute(params));
  }
  assert.equal(outbound, 0);
  checks.invalid_queries_and_url_override_never_leave_process = true;

  globalThis.fetch = async () => { outbound++; return new Response("limited", { status: 429 }); };
  await assert.rejects(extension().execute({ query: "公开问题" }));
  assert.equal(outbound, 1);
  checks.rate_limit_does_not_retry_or_fall_back = true;

  globalThis.fetch = async () => sse(999, { protocolVersion: "2025-03-26" });
  await assert.rejects(extension().execute({ query: "公开问题" }));
  globalThis.fetch = async () => new Response(new Uint8Array(96 * 1024 + 1), { headers: { "content-type": "text/event-stream" } });
  await assert.rejects(extension().execute({ query: "公开问题" }));
  checks.mismatched_response_and_oversized_body_rejected = true;

  const cancellation = new AbortController();
  let aborted = false;
  globalThis.fetch = async (_url, options) => new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => { aborted = true; reject(options.signal.reason); }, { once: true });
  });
  const pending = extension().execute({ query: "公开问题" }, cancellation.signal);
  cancellation.abort(new Error("cancelled by proof"));
  await assert.rejects(pending);
  assert.equal(aborted, true);
  checks.cancellation_aborts_network_work = true;
} finally {
  globalThis.fetch = nativeFetch;
}
if (process.argv.includes("--live")) {
  const result = await extension().execute({ query: "Rust programming language official website" });
  assert.match(result.content[0].text, /https:\/\//);
  assert.match(result.content[0].text, /Rust/i);
  checks.live_free_search_without_api_key = true;
  console.log(result.content[0].text);
}
console.log(JSON.stringify({ passed: true, checks }, null, 2));
