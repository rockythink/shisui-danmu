import readline from "node:readline";
import { once } from "node:events";

const MAX_BUFFERED_BYTES = 1024 * 1024;
const STATUS_EVENTS = new Set([
  "auto_retry_start",
  "auto_retry_end",
  "auto_compaction_start",
  "auto_compaction_end",
]);

let active = false;
let buffered = [];
let bufferedBytes = 0;
let overflowed = false;
let finalAttemptFailed = false;
let failureReason;
let errorSequence = 0;

async function write(line) {
  if (!process.stdout.write(line + "\n")) {
    await once(process.stdout, "drain");
  }
}

function clearAttempt() {
  buffered = [];
  bufferedBytes = 0;
  overflowed = false;
  finalAttemptFailed = false;
  failureReason = undefined;
}

function rememberText(event) {
  if (overflowed) return;
  const line = JSON.stringify({
    type: "message_update",
    assistantMessageEvent: {
      type: "text_delta",
      delta: event.assistantMessageEvent.delta,
    },
  });
  const bytes = Buffer.byteLength(line) + 1;
  if (bufferedBytes + bytes > MAX_BUFFERED_BYTES) {
    buffered = [];
    bufferedBytes = 0;
    overflowed = true;
    return;
  }
  buffered.push(line);
  bufferedBytes += bytes;
}

function safeReason(value) {
  if (typeof value !== "string") return undefined;
  let reason = value.replace(/[\u0000-\u001f\u007f]+/g, " ").replace(/\s+/g, " ").trim();
  if (!reason || /api[_ -]?key|authorization|bearer|token|credential|secret/i.test(reason)) return undefined;
  reason = Array.from(reason).slice(0, 160).join("");
  return reason || undefined;
}

function errorNotification(message) {
  errorSequence += 1;
  return JSON.stringify({
    type: "extension_ui_request",
    id: `shisui-pi-error-${errorSequence}`,
    method: "notify",
    message,
    notifyType: "error",
  });
}

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    await write(line);
    continue;
  }
  const type = typeof event?.type === "string" ? event.type : "";
  if (type === "agent_start") {
    if (!active) clearAttempt();
    active = true;
    await write(line);
    continue;
  }
  if (type === "auto_retry_start" || type === "auto_compaction_start") {
    clearAttempt();
    continue;
  }
  if (STATUS_EVENTS.has(type)) continue;
  if (active && ((type === "message_update" && event.assistantMessageEvent?.type === "toolcall_start") || type === "tool_execution_start")) {
    buffered = [];
    bufferedBytes = 0;
    overflowed = false;
  }
  if ((type === "message_end" || type === "turn_end") && event.message?.role === "assistant") {
    finalAttemptFailed = event.message.stopReason === "error";
    failureReason = finalAttemptFailed ? safeReason(event.message.errorMessage) : undefined;
  }
  if (type === "agent_settled" && active) {
    if (overflowed || finalAttemptFailed) {
      const message = overflowed
        ? "Pi模型响应超过1MiB安全缓冲上限"
        : failureReason ? "Pi模型请求失败：" + failureReason : "Pi模型请求失败";
      await write(errorNotification(message));
    } else {
      for (const pending of buffered) await write(pending);
    }
    clearAttempt();
    active = false;
    await write(line);
    continue;
  }
  if (active && type === "message_update" && event.assistantMessageEvent?.type === "text_delta") {
    rememberText(event);
    continue;
  }
  await write(line);
}
