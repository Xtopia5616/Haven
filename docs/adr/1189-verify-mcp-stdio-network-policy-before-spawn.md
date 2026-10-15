# ADR 1189：验证 MCP stdio 网络策略先于进程启动

## 状态

已采纳并实施。

## 背景

MCP stdio 子进程可能自行访问网络，Haven 无法验证其目的地址。`McpClient::connect` 已要求此类连接使用 `NetworkPolicy::Open`，但原有测试只检查错误和 Offline 状态；如果未来有人把策略判断移到 spawn 后，这些断言仍会通过并启动外部进程。

## 决定

- 在 `Ask`、`Restricted`、`Deny` 三种非 Open 策略下，连接入口必须在启动已配置的 stdio command 前拒绝。
- 回归测试使用当前 Rust 测试程序作为假的 stdio server；被启动时，它会在临时目录创建哨兵文件。测试断言各策略均返回错误、状态保持 Offline、哨兵不存在。
- MCP stdio 只在用户显式选择 Open 时才进入现有连接/工具调用链。该策略是授权边界，不是 Windows OS 网络隔离；Open 下子进程仍可使用当前用户允许的网络能力。

## 替代方案

- 只断言错误字符串和状态：拒绝，spawn 后握手失败也会得到相同结果，无法证明拒绝发生在外部副作用前。
- 在测试中打开真实网络服务：拒绝，测试只需证明是否启动本地子进程，无需网络依赖。

## 影响与迁移

仅新增测试，不改变运行时行为、配置或数据库，无需数据重置。

## 验证

- MCP 测试覆盖三种非 Open 策略及真实 spawn 哨兵。
- 运行 MCP crate 测试/check/严格 Clippy、workspace Rust 测试/check/严格 Clippy、格式检查、ADR 索引、crate dependency inventory 和 `git diff --check`。
