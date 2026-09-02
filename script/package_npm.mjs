#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { chmod, copyFile, mkdir, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const version = (process.argv[2] ?? "").replace(/^v/, "");
const artifactsDirectory = path.resolve(process.argv[3] ?? path.join(repositoryRoot, "dist"));
const outputDirectory = path.resolve(
  process.argv[4] ?? path.join(repositoryRoot, "dist", "npm", "danmu-tui"),
);

if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error("用法：node script/package_npm.mjs <版本> [Release 制品目录] [输出目录]");
}

const targets = [
  ["darwin-arm64", "shisui-danmu-macos-aarch64.tar.gz", "danmu"],
  ["darwin-x64", "shisui-danmu-macos-x86_64.tar.gz", "danmu"],
  ["linux-arm64", "shisui-danmu-linux-aarch64.tar.gz", "danmu"],
  ["linux-x64", "shisui-danmu-linux-x86_64.tar.gz", "danmu"],
  ["win32-x64", "shisui-danmu-windows-x86_64.zip", "danmu.exe"],
];

await rm(outputDirectory, { recursive: true, force: true });
await mkdir(path.join(outputDirectory, "bin"), { recursive: true });
await mkdir(path.join(outputDirectory, "vendor"), { recursive: true });

for (const [target, archiveName, executableName] of targets) {
  const archive = path.join(artifactsDirectory, archiveName);
  await stat(archive);
  const destination = path.join(outputDirectory, "vendor", target);
  await mkdir(destination, { recursive: true });
  if (archiveName.endsWith(".zip")) {
    execFileSync("unzip", ["-oq", archive, "-d", destination], { stdio: "inherit" });
  } else {
    execFileSync("tar", ["-xzf", archive, "-C", destination], { stdio: "inherit" });
  }
  const executable = path.join(destination, executableName);
  await stat(executable);
  if (executableName !== "danmu.exe") {
    await chmod(executable, 0o755);
  }
}

await copyFile(
  path.join(repositoryRoot, "packaging", "npm", "bin", "danmu.cjs"),
  path.join(outputDirectory, "bin", "danmu.cjs"),
);
await chmod(path.join(outputDirectory, "bin", "danmu.cjs"), 0o755);
for (const file of ["README.md", "LICENSE", "THIRD_PARTY_NOTICES.md"]) {
  await copyFile(path.join(repositoryRoot, file), path.join(outputDirectory, file));
}

const packageJson = {
  name: "danmu-tui",
  version,
  description: "为知识型主播收束弹幕、问题与现场控制的原生终端工作台",
  license: "MPL-2.0",
  author: "Elazer <apps@elazer.wang>",
  homepage: "https://github.com/rockythink/shisui-danmu#readme",
  repository: {
    type: "git",
    url: "git+https://github.com/rockythink/shisui-danmu.git",
  },
  bugs: {
    url: "https://github.com/rockythink/shisui-danmu/issues",
  },
  keywords: ["bilibili", "danmaku", "tui", "streaming", "obs"],
  preferGlobal: true,
  engines: {
    node: ">=18",
  },
  bin: {
    danmu: "bin/danmu.cjs",
  },
  files: ["bin", "vendor", "README.md", "LICENSE", "THIRD_PARTY_NOTICES.md"],
  publishConfig: {
    access: "public",
  },
};
await writeFile(
  path.join(outputDirectory, "package.json"),
  `${JSON.stringify(packageJson, null, 2)}\n`,
);

console.log(`已生成 npm 包目录：${outputDirectory}`);
