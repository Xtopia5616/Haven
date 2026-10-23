# ADR 1375：将可复用授权绑定到资源范围

## 状态

已实施 — 2026-10-23。

## 背景

ADR 1374 为 Files 路径、显式 Shell 工作目录和 HTTP origin 引入了 typed 资源声明，但 `Session` / `Always` 授权仍只按 capability 匹配。一个目录或网络目的地上的确认因此可能被复用到同一权限键的其它资源。窗口、剪贴板、注册表和 MCP 远程对象尚无完整 typed 声明。

## 决定

1. 可复用 allow 同时匹配 capability 和已声明资源。授权请求没有可信 typed 资源时只能选择本次允许；后端拒绝保存空资源范围的 allow。`Operation` / `Group` / `Tool` 继续选择 capability 粒度，不能扩大资源范围。
2. 持久资源范围支持本地路径（读取/写入、精确路径/目录子树）和网络 origin（scheme、host、port）。目录选择由后端针对当前声明解析：现存目录授权自身及子项，文件授权其父目录及子项；拒绝 UNC/device 路径，并在授权创建和每次匹配时拒绝重解析点路径。
3. 确认 UI 显示后端 typed 声明的路径或 origin。文件路径可选精确资源或目录子树；网络范围始终是精确 origin。Session 与 Always allow 均要求该资源范围。对没有资源声明的操作，UI 只提供本次 allow；deny 仍按 capability 生效。
4. 同一 capability、同一 effect 的重复允许合并已明确批准的资源范围；effect 切换时替换旧决策。Session grants 在数据库事务中合并，Always grants 在配置原子写入后发布。拒绝规则仍优先于允许规则。
5. `session_authorization_grants` 增加 JSON `resource_scope` 列，数据库 schema 升至 v61。Haven 不对旧数据库做运行时迁移，必须按 `docs/release-and-reset.md` 备份并重置。旧 `config.toml` 中缺少资源范围的 Allow 会被删除并要求重新授权；旧 Deny 保留。
6. 尚无可信 typed 声明的窗口、剪贴板、注册表、MCP 对象及其它操作不能获得可复用 allow。不得根据通用 JSON 参数、显示名称或 tool name 推测这些资源的身份。其 typed 覆盖由后续切片补齐。

## 替代方案

- 继续将授权仅按 capability 匹配，会保留跨路径和跨目的地复用风险。
- 将所有旧 Allow 转成“所有资源”兼容规则无法安全缩窄，故删除并要求用户重新确认。
- 拒绝所有持久授权会失去对已有可信 path/origin 声明的实用性；因此只在声明缺失时 fail closed。
- 让 Deny 也必须指定资源会削弱现有拒绝规则的保守性；此变更保留 capability 级 Deny。

## 影响与验证

确认收据和执行 proof 仍绑定精确资源声明；grant 匹配要求请求中的每项资源都被持久范围覆盖。设置页显示已保存的资源范围。正向与负向测试覆盖资源匹配、目录边界、网络 origin、旧无范围 allow、授权 scope 合并及数据库读写。

窗口、剪贴板、注册表、MCP 远程对象和其它进程能力仍需新增可信资源声明及其执行前验证。本 ADR 不声称这些能力已经获得对象级授权；目前它们无法保存可复用 Allow。

回滚到不认识 schema v61 的二进制前，必须从完整备份恢复旧数据库。配置回滚时，已删除的无范围 Allow 无法自动恢复；用户须在重新授权后保存新配置。
