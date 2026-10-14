# ADR 1158：在 provider 请求前有界重查 steering

- 日期：2026-10-14
- 状态：已实现

## 背景

Turn 开始时注入输入后，before_step 的压缩、目录准备和请求投影可能耗时。此期间接受的 steering 原本要等模型请求结束后才生效。ADR 1156 已区分普通 turn 与工作完成边界；本切片补充请求前的 steering 阶段。

## 决定

- Actor drain 增加 `ContextBoundary::BeforeRequest`，仅消费 steering 队列的一批。复用每批 16 项、16 Ki 字符、8 MiB 附件的预算；尾项保留。Ask answer、follow-up、后台结果和 inbox 不在此阶段消费。
- 顺序为 turn 输入 → before_step → 工具目录/请求投影/路由准备 → 取消及 deadline 检查 → 单次 late steering drain → 如有新增内容则重新校验预算、必要时压缩并重建请求投影、媒体策略与路由 → provider。
- Late steering 仍通过 apply_transcript 持久提交并发布；请求只使用提交后的 canonical 投影。重建复用已冻结的工具目录 token estimate，并重新估算消息 tokens；不重新执行 before_step，不重复 memory-trigger 或 inbox 领取。
- 每 turn 只进行一次 late poll，重建/再次压缩期间到达的输入留到下一安全边界。取消不启动 provider；所有阶段沿用同一 turn deadline。
- deadline 到达时先取消 provider/tool 工作；外层最多保留一秒收尾窗口，让工具批次按顺序持久化已完成结果及已启动调用的 unknown 结果。收尾后仍报告 deadline 错误；父 session 的用户取消不被误判为 deadline。
- Follow-up 仍只在工作完成边界消费；Ask answer 的消费资格独立于 follow-up，继续由普通 turn 阶段处理。

## 替代方案与风险

仅在 before_step 后、请求投影前读取，无法覆盖目录/路由准备期间的新输入；无限重复检查可能饿死 provider。选择单次有界补读。主要风险是旧预算或媒体路由被复用、取消后仍发请求以及重复触发 hook 副作用，使用行为测试覆盖。

本切片仅改变内部消费阶段和请求编排，无新增依赖、权限路径、持久数据结构或 IPC 变更；无需额外重置。ADR 1156 的待投递 marker 契约不变，当前数据库版本及重置范围以 release-and-reset.md 为准。turn/actor 继续由既有 owner 管理，新增测试独立放在 request_preparation_tests.rs，避免继续扩张已有测试大文件。

## 验证与回滚

新增测试覆盖准备期间输入进入首次请求、单批上限、压缩后不再轮询、取消、队列隔离，以及 deadline 时单个/并行工具结果的持久修复。`cargo fmt --all -- --check`、Rust workspace check、严格 Clippy、workspace tests、UI check/1285 项测试/生产构建和 IPC 事件/生成契约检查均通过。回滚本切片不需要数据重置，保留 ADR 1156 的既有边界即可。
