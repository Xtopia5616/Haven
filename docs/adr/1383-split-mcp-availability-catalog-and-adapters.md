# ADR 1383：拆分 MCP 可用性、目录展示和调用适配

## 状态

已实施 — 2026-10-23。完成执行上下文 Slice 4；Code Mode 子调用继续复用现有授权、MCP 远程目标契约与 durable journal。

## 背景

`McpToolExposure` 的 `direct` / `model_only` / `deferred` / `codemode` / `hidden` 同时表达是否允许调用、schema 的发现方式和执行入口。目录、`load_mcp`、provider-native MCP tool surface、Code Mode 子调用和 stale overlay 校验都需要解释同一模式；server 级 Legacy/CodeMode 切换还把独立决策叠在一起。Code Mode 已有独立 bounded adapter，且每个子调用已通过普通 Tools 授权、MCP typed remote resource 和 durable journal；本切片不新建授权 owner。

## 决定

1. 每项 MCP 工具配置保存三个独立维度：
   - `availability`: `enabled` / `disabled`；只有启用项可经 adapter 路由。
   - `catalog`: `direct` / `deferred` / `hidden`；决定直接展示、按需发现/加载，或仅从通用目录隐藏。
   - `invocation`: `none` / `native` / `codemode` / `native_and_codemode`；决定可用的执行适配器。
2. 原生 provider surface 要求 `enabled + native adapter`，且 catalog 不能是 `deferred`；`direct` 和 `hidden` 的 native adapter 都直接进入 provider surface，区别仅是 `hidden` 不出现在通用目录。`load_mcp` 和 deferred session overlay 要求 `enabled + deferred + native adapter`。Code Mode 子调用要求 `enabled + codemode adapter`，不以目录状态代替调用授权。需要阻止调用时必须禁用能力。无 adapter 的工具不会被发到调用 surface。
3. 两种 adapter 可以同时启用；它们都进入现有 `ToolAuthorizationContract`、同一 typed MCP 远程资源和 Code Mode journal，不因调用通道增加第二份 grant、target 或重试状态。
4. 新配置默认 `enabled + deferred + native`。per-tool exact/wildcard resolver 保留精确匹配优先、最长字面 pattern 优先和稳定字典序 tie-break。
5. 配置加载时做一次保守迁移并写回新对象形状：
   - `direct` / `model_only` → `enabled + direct + native`。当前实现中两值都只进入原生 surface，且都不进入 Code Mode 子调用，因此转换后权限等强。
   - `deferred` → `enabled + deferred + native`。
   - `codemode` → `enabled + hidden + codemode`。
   - `hidden` → `disabled + hidden + none`，保留“不可发现也不可调用”的旧语义。
   未知旧值仍使整份配置按现有策略备份并进入只读恢复；不做猜测或扩大调用通道。转换只作用于 `[mcp_servers].exposure.default` 和 per-tool overrides。
6. 设置页分别编辑可用性、目录行为和适配器，覆盖规则格式为 `pattern=availability,catalog,invocation`。旧 wire enum 与 server 级 Legacy/CodeMode 模式选项删除。

## 配置与数据影响

`config.toml` 中 exposure 字段从字符串 enum 改为含三个字段的对象。ConfigLoader 只转换已知旧值，并在成功加载后原子持久化规范形状；迁移失败保留原文件并使用现有备份/只读恢复行为。无需重置数据库、inbox 或凭据。MCP 连接协议、tool 参数和授权/日志持久格式不变。

## 替代方案

- 保留旧枚举并仅补 resolver helper：拒绝；同一个值仍会跨维度解释，UI 仍不能表达两个 adapter 同时可用。
- 把 `hidden` 只映射到目录隐藏：拒绝；旧 hidden 明确不可调用，转换必须同时禁用。
- 将 `model_only` 转成 `codemode` 或开放 Code Mode：拒绝；旧值没有 Code Mode adapter，不得扩大调用通道。
- 按 server 选择 Legacy/CodeMode：拒绝；这是 tool-level adapter 支持，且会使同一能力的调用契约再次受 server 配置耦合。

## 验收与限制

覆盖三维策略序列化、exact/wildcard override、native/deferred/codemode/both 路由、availability fail-closed、stale native/Code Mode 调用撤销和 legacy config 转换。跨端 DTO 由 Rust command contract 生成；Settings UI 对新对象作运行时 shape 校验。未改数据库 schema，源代码回滚即可恢复实现；已迁移配置可从迁移前备份恢复，或按[仅重建配置](../release-and-reset.md#仅重建配置)重建，无需清除用户数据。

### Windows 部分验收（2026-10-24；Acceptance Open）

在 Windows 11 Insider 29683、Rust 1.99.0、Node 24.21.0、pnpm 12.12.1 上，以 `527bcd3336cc6d6526c3ce6a172f778918b0dbae` 为基线，通过 `cargo tauri dev --config crates/app-binary/tauri.dev.conf.json` 在独立 `HAVEN_DATA_DIR` 启动开发版。禁用的旧配置 fixture 覆盖五种 exposure 值；启动迁移将 `deferred` 转为 `enabled + deferred + native`，`direct` / `model_only` 转为 `enabled + direct + native`，`hidden` 转为 `disabled + hidden + none`，`codemode` 转为 `enabled + hidden + codemode`。五种结果均通过精确持久化断言；测试 server 保持 disabled，未启动 MCP 进程或访问网络。数据库 schema v61，SQLite integrity check 返回 `ok`。

首次 GUI 验收被 Node.js Windows 防火墙权限提示挡住，提示随后由用户手动处理；自动化未操作该提示或改变防火墙规则。2026-10-24 在仅包含三份验收文档增量的 `6f60ef1` 上，沿用原隔离 profile 重新执行上述开发版启动命令，验证已持久化迁移配置能再次加载。

真实 Windows UI 验证：首屏与 Settings 正常显示；工具 → MCP 卡片显示测试 server“已停用”“已断开”、默认“可调用 · 按需发现 · 原生调用”和“4 条覆盖”。打开编辑器后，Availability、Catalog、Invocation 分别显示 `enabled`、`deferred`、`native`；四条覆盖文本与迁移结果一致。只展开菜单核对选项：Availability 为 enabled/disabled，Catalog 为 direct/deferred/hidden，Invocation 为 none/native/codemode/native_and_codemode，未选择新值。取消后 server 仍 disabled；配置 SHA-256 与首次迁移后的 `886c4d51bf472bc9cabc61a40668da6dec6665b14a556c3c54ee3aae2ea1afbe` 相同，未发生配置写入或 MCP 启动。

当前 Windows Computer Use 执行限制禁止自动修改应用内安全/隐私设置，因此未通过 UI 修改并保存 availability/invocation；“已迁移配置重启读取”不等于“编辑器修改后保存/重启”通过。native/Code Mode stale-call 行为和未知旧值恢复也仍未实测，Slice 4 保持 Acceptance Open。复现界面读取可沿用该禁用 fixture：启动隔离开发版 → 工具 → MCP → 编辑 → 展开三个菜单 → Cancel；不得把静态选项展示当作真实调用路由验收。
