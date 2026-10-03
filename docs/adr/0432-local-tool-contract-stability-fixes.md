# ADR 0432：本机工具契约稳定性修正

## 状态

已接受（2026-10-03）。

## 背景

一次本机工具稳定性检查发现 8 个确定性契约缺口：文件写入不补齐父目录、删除不支持空目录、缺少 fast-chat route 时摘要不可用、进程列表输出过大且进程名序列化为字节数组、`checklist.add` 忽略 `done`、环境变量列表用 `name` 承载前缀、无脚本 Skill 被当作可执行工具加载，以及 PowerShell 引号/退出码行为缺少对应回归契约。

## 决定

1. `files.write` 在 expected-hash 校验和 dry-run 返回之后创建父目录，再通过现有原子替换写文件；CAS 失配和 dry-run 不创建目录。
2. `files.delete` 可删除普通文件/链接及空目录。目录删除不递归，非空目录维持安全失败。
3. `files.summary` 优先使用 `FastChat`；未配置时回退到 `Chat`，两个 route 都不可用时才报告摘要不可用。结果携带实际 `request_kind`。
4. `process.list` 将进程名序列化为字符串，支持不区分大小写的 `name_filter` 和 `limit`（默认 50、最大 200），并报告匹配数、返回数和截断提示。
5. `checklist.add` 使用可选 `done` 初始化状态，默认仍为未完成。
6. `system.env.list` 的正式筛选参数为 `prefix`；get/set/unset 的变量名继续使用 `name`。聚合执行边界仍接受原有 name 前缀作为内部兼容输入。
7. 无入口脚本的 Skill 不投影为已启用，不进入可加载工具目录，也不能通过设置启用；创建 executable Haven Skill 必须提供非空 Python 脚本。UI 显示不可执行原因并禁用开关。
8. PowerShell 继续通过 `-EncodedCommand` 传递完整脚本；回归覆盖内嵌引号、`+` 表达式、真实非零退出码与完整输出日志路径。

## 替代方案

- 递归删除目录：拒绝。`files.delete` 不隐含递归破坏行为；需要时应使用清晰、单独受控的操作。
- 无 fast-chat 时直接失败：拒绝。用户已请求摘要且 default chat 可用时应继续完成。
- 将无脚本 Skill 当成空脚本执行：拒绝。空执行无法实现 Skill 指令，会掩盖配置错误。
- 把进程列表永久截断为固定 200 行：拒绝。小默认上限和显式 name filter/limit 能控制模型输出并保留按需扩展。

## 影响与验证

变更仅涉及工具执行逻辑、模型可见 schema/提示、技能管理 UI 和回归测试；无数据库迁移或用户数据重置。已有无脚本 Skill 仍可在设置页看到，但按不可执行状态展示，补齐入口脚本并刷新后即可启用。

验证覆盖文件父目录/CAS/dry-run、空目录与非空目录删除、fast-chat 回退、进程 JSON/filter/limit、checklist 初始状态、环境变量 prefix schema 和执行、无脚本 Skill 启用拒绝，以及 PowerShell 引号与退出状态。门禁按 `docs/git-workflow.md` 执行。

## 回滚

回滚本 ADR 对应代码、提示、UI、测试和文档即可。无需重置数据库或配置；若回滚到不识别新工具参数的旧二进制，重新加载 builtin tool catalog。
