#!/usr/bin/env python3
"""Print host MCP configuration (Pi: CLI example). Never install or change settings."""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--host', required=True, choices=['omp', 'claude', 'codex', 'opencode', 'gemini', 'cursor', 'vscode', 'amp', 'pi'])
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--instance', required=True, type=Path)
args = parser.parse_args()
if not args.binary.is_absolute() or not args.instance.is_absolute():
    parser.error('binary and instance must be absolute paths')
if not args.binary.is_file():
    parser.error('binary must be an existing executable file')
command = str(args.binary)
argv = ['--instance', str(args.instance), 'mcp']
server = {'type': 'stdio', 'command': command, 'args': argv}
if args.host == 'pi':
    print(json.dumps({'integration': 'cli_example_not_mcp', 'command': command, 'args': ['--instance', str(args.instance), 'agent', 'status', '{}'], 'note': 'Pi core has no native MCP configuration. Read the shared Skill and use the native permitted CLI tool; the in-danmu Pi Runner does not need MCP.'}, indent=2, ensure_ascii=False))
elif args.host == 'amp':
    server.pop('type')
    print(json.dumps({'amp.mcpServers': {'shisui_danmu': server}}, indent=2, ensure_ascii=False))
elif args.host == 'codex':
    # JSON basic strings/arrays also have valid TOML syntax for these path values.
    print('[mcp_servers.shisui_danmu]')
    print('command = ' + json.dumps(command, ensure_ascii=False))
    print('args = ' + json.dumps(argv, ensure_ascii=False))
    print('startup_timeout_sec = 10\ntool_timeout_sec = 40')
elif args.host == 'opencode':
    print(json.dumps({'mcp': {'shisui_danmu': {'type': 'local', 'command': [command] + argv, 'enabled': True, 'timeout': 40000}}}, indent=2, ensure_ascii=False))
elif args.host == 'vscode':
    print(json.dumps({'servers': {'shisui_danmu': server}}, indent=2, ensure_ascii=False))
else:
    if args.host == 'gemini':
        server.pop('type')
        server.update(timeout=40000, trust=False)
    elif args.host == 'omp':
        server['timeout'] = 40000
    print(json.dumps({'mcpServers': {'shisui_danmu': server}}, indent=2, ensure_ascii=False))
