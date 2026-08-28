# 参与贡献

感谢你改进拾穗弹幕台 TUI。

## 范围

本仓库只接受以下范围的修改：

- `DanmuCore` 平台无关领域行为；
- `BilibiliDanmu` 平台 Adapter；
- `OBSControl` 有限 OBS 遥控；
- `ShisuiDanmuTerminal` 终端界面；
- 与上述模块直接相关的测试、文档和发行脚本。

商业 GUI、播放器、完整直播画布、音频电平和 OBS 场景编辑器不属于本仓库。

## 开发环境

- macOS 14 或更高版本；
- Swift 6 工具链。

## 提交修改

1. 从 `main` 创建短生命周期分支。
2. 保持平台协议字段在 Adapter 内，不要让领域模块依赖 B 站原始 payload。
3. 为新增或改变的可观察行为补充测试。
4. 运行完整门禁：

   ```bash
   ./script/verify.sh
   ```

5. Pull Request 说明用户可见变化、验证方式和可能影响的本地数据或凭据边界。

不要提交 Cookie、CSRF token、Keychain 内容、真实直播账号信息或本机会话日志。安全问题请按 [SECURITY.md](SECURITY.md) 私下报告。

## License

提交贡献即表示你有权按本仓库的 [Mozilla Public License 2.0](LICENSE) 提供该贡献。项目名称、Logo 和其他品牌标识仍受 [TRADEMARKS.md](TRADEMARKS.md) 约束。
