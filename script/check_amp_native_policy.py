#!/usr/bin/env python3
"""Inspect a macOS Amp bundle without running Amp or contacting its service.

Usage: python3 script/check_amp_native_policy.py /absolute/path/to/native/amp
The extracted functions are the native tool filter, not an ACP simulator.
"""
import json
import pathlib
import struct
import subprocess
import sys


def source_from_macho(path):
    with path.open("rb") as binary:
        header = binary.read(32)
        magic, _, _, _, count, _, _, _ = struct.unpack("<8I", header)
        if magic != 0xFEEDFACF:
            raise ValueError("Expected a little-endian 64-bit Mach-O native Amp")
        for _ in range(count):
            position = binary.tell()
            command, size = struct.unpack("<2I", binary.read(8))
            payload = binary.read(size - 8)
            if command == 0x19 and payload[:16].rstrip(b"\0") == b"__BUN":
                offset, length = struct.unpack_from("<2Q", payload, 32)
                binary.seek(offset)
                bundle = binary.read(length)
                return bundle[8:].decode("utf-16-le", errors="ignore")
            binary.seek(position + size)
    raise ValueError("Missing Amp Bun bundle; do not infer a compatible safety contract")


def main():
    source = source_from_macho(pathlib.Path(sys.argv[1]))
    start = source.index("function Tf(")
    end = source.index("function Ert(", start)
    filters = source[start:end]
    if "function CI(" not in filters:
        raise ValueError("Native tool filter layout changed; re-audit this version")
    # The permission runtime cannot spawn anything, even if this probe regresses.
    probe = filters + """
const assert = require('node:assert/strict');
for (const name of ['Bash', 'Read', 'edit_file', 'web_search', 'mcp__arbitrary__tool']) {
  const result = CI({name, source:'builtin'}, {settings:{'tools.disable':['*']}});
  assert.deepEqual(result, {enabled:false, disabledReason:'settings'});
}
assert.equal(CI({name:'Bash',source:'builtin'},{settings:{'tools.enable':[]}}).enabled,true);
console.log('Native Amp filter: wildcard disables every tested tool; empty allowlist does NOT disable tools.');
"""
    subprocess.run(["node", "--permission", "-e", probe], check=True)
    for expected in [
        'Ignoring AMP_DISABLE_PLUGINS outside development',
        'command:process.execPath,env:{BUN_BE_BUN:"1"}',
    ]:
        if expected not in source:
            raise ValueError("Native plugin launch contract changed; re-audit this version")
    node = pathlib.Path(subprocess.check_output(
        ["node", "-p", "process.execPath"], text=True).strip()).resolve()
    profile = '(version 1)(allow default)(deny process-fork)(deny process-exec)(allow process-exec (literal ' + json.dumps(str(node)) + '))(deny appleevent-send)'
    no_child = """
const assert = require('node:assert/strict');
const result = require('node:child_process').spawnSync(process.execPath, ['-e', 'process.exit(0)']);
assert.equal(result.error?.code, 'EPERM');
assert.equal(result.status, null);
console.log('OS sandbox: same-binary child execution is denied (EPERM).');
"""
    subprocess.run(["/usr/bin/sandbox-exec", "-p", profile, str(node), "-e", no_child], check=True)
    print("PASS: no Amp process, browser, login, or model request was started.")


if __name__ == "__main__":
    main()
