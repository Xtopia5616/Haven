# ADR 0245：会话错误原因缓存归 SessionReducer

> `sessionErrorStore` 转发层已由 [ADR 0376](0376-session-ui-obsolete-compatibility-removal.md) 删除；本 ADR 中保留兼容 facade 的决定已被后续实现取代。
>
> 活动错误展示与 run-end notice 曾作为 `error` / `termination` 两份状态并存；该部分由 [ADR 0638](0638-merge-session-run-end-ui-state.md) 收敛。按 session 保存历史错误原因的 `sessionErrorReasons` 决定仍有效。

- 状态：已采纳（2026-09-24）
- 范围：UI 进程内的历史会话错误原因
- 关联：架构降复杂度重构路线图阶段 8

## 背景

`SessionReducer` 已拥有活动会话的错误展示状态，而独立的 `sessionErrorStore` 又以 `Record<sessionId, reason>` 保存同一错误事件的原因，供历史页重新打开错误会话时恢复提示。两个容器需要分别维护按会话索引、规范化、查找和清除行为。

## 决定

1. 将 `sessionErrorReasons` 加入 `SessionReducerState`，由应用级 `appSessionReducer` 作为唯一运行时 owner；以 `session/error-reason-remembered` 和 `session/error-reason-forgotten` 表达写入与删除。
2. 保留 `rememberSessionError`、`forgetSessionError`、`getSessionErrorReason` 的调用接口。`sessionErrorStore` 仅作为兼容 facade，所有操作委托给 reducer。
3. reducer 只保存 trim 后的非空原因；相同规范化值和未知删除是无操作，读取未知 session 返回空字符串。原因按 session 隔离。
4. 该映射只存在于前端内存状态。它不加入持久化格式、Rust DTO、IPC contract 或 backend session state。
5. 保留原生命周期：事件 handler 在会话进入 busy 状态时清除该 session 的缓存；会话删除或清空列表本身不清除此映射。活动错误展示 `error`/`termination` 与历史原因缓存仍是不同语义，不互相驱动或合并。

## 明确不做

本切片不调整 `session:error` handler、历史恢复 fallback、删除会话清理策略或 `error` 与 `termination` 的语义；不提取 ChatController，不改页面编排、IPC、持久化或其他文件边界。

## 验证与回滚

独立 Vitest 覆盖 trim、空值、会话隔离、重复写入、清除、未知读取，以及删除会话/清空列表时保留缓存；既有 reducer 行为测试继续执行。实施门禁为 UI 全量测试与 Svelte 检查。

回滚时恢复 `sessionErrorStore` 的本地 writable store，并移除 reducer 字段与两个 action 及本 ADR/索引/路线图记录；不涉及持久化数据或迁移。
