# 参与贡献

感谢你改进拾穗弹幕台 TUI。

## 范围

本仓库只接受以下范围的修改：

- `src/domain`：平台无关领域行为；
- `src/bilibili`：B 站平台 Adapter；
- `src/obs.rs`：有限 OBS 遥控 Adapter；
- `src/terminal`：终端渲染与输入交互；
- `src/config.rs`、`src/persistence.rs`、`src/storage.rs`：配置与本地数据；
- 与上述 Module 直接相关的测试、文档和发行脚本。

商业 GUI、播放器、完整直播画布、音频混音台和 OBS 场景编辑器不属于本仓库。

## 开发环境

- Rust 1.89 或更高版本；
- macOS、Linux 或 Windows；
- `rustfmt` 与 `clippy` 组件。

## 提交修改

1. 从 `main` 创建短生命周期分支。
2. 保持平台协议字段在 Adapter 内，不要让领域 Module 依赖 B 站原始 payload。
3. 为新增或改变的可观察行为补充测试。
4. 运行完整门禁：

   ```bash
   ./script/verify.sh
   ```

5. Pull Request 说明用户可见变化、验证方式和可能影响的本地数据或凭据边界。

不要提交 Cookie、CSRF token、系统凭据存储内容、真实直播账号信息或本机会话日志。安全问题请按 [SECURITY.md](SECURITY.md) 私下报告。

## License

提交贡献即表示你有权按本仓库的 [Mozilla Public License 2.0](LICENSE) 提供该贡献。项目名称、Logo 和其他品牌标识仍受 [TRADEMARKS.md](TRADEMARKS.md) 约束。
