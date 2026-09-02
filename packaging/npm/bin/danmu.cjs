#!/usr/bin/env node

"use strict";

const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");
const path = require("node:path");

const targets = {
  "darwin-arm64": ["darwin-arm64", "danmu"],
  "darwin-x64": ["darwin-x64", "danmu"],
  "linux-arm64": ["linux-arm64", "danmu"],
  "linux-x64": ["linux-x64", "danmu"],
  "win32-x64": ["win32-x64", "danmu.exe"],
};

const target = targets[`${process.platform}-${process.arch}`];
if (!target) {
  console.error(
    `DANMU 暂不支持 ${process.platform}/${process.arch}；请从 https://github.com/rockythink/shisui-danmu/releases 下载兼容版本。`,
  );
  process.exit(1);
}

const executable = path.join(__dirname, "..", "vendor", target[0], target[1]);
if (!existsSync(executable)) {
  console.error(`DANMU 原生程序缺失：${executable}。请重新安装 danmu-tui。`);
  process.exit(1);
}

const result = spawnSync(executable, process.argv.slice(2), {
  stdio: "inherit",
  env: process.env,
});

if (result.error) {
  console.error(`DANMU 启动失败：${result.error.message}`);
  process.exit(1);
}
if (result.signal && process.platform !== "win32") {
  process.kill(process.pid, result.signal);
}
process.exit(result.status ?? 1);
