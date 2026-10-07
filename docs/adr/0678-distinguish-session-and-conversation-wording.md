# ADR 0678：区分 Session 实体与 conversation 内容用词

## 背景

`docs/naming.md` 已规定 `session` 表示持久会话实体及其运行状态；`conversation` 用于自然语言交流内容、历史 transcript 和模型上下文。但部分 UI/backend 注释仍用 conversation 指代当前、暂停、已恢复或选中的 session，掩盖了状态 owner。工具使用说明中的 “Haven conversation state” 也实际指 Agent session state。

代码里剩余的 conversation 用法包含模型上下文、历史 transcript、用户可读措辞、Logo 对话气泡和外部 xAI conversation-affinity 协议，这些保留各自准确语义。

## 决定

- 将指代可持久 session、session 状态或 session-scoped UI 的源码注释改为 `session`。
- 对消息顺序、流式文本和渲染内容使用 `transcript`；对视觉界面使用 `chat`。
- Agent 工具用途说明将 “Haven conversation state” 改为 “Haven session state”，并同步更新源码断言。
- 不更改 Rust/TypeScript 符号、IPC、数据库字段或产品显示文本；模型 prompt 的这一处术语修正保持原有禁止行为含义。

## 考虑过的方案

- 将所有 `conversation` 替换成 `session`：这会错误命名自然语言内容、模型上下文和外部 provider 字段。
- 保留状态注释中的 conversation：代码规范已区分实体与 transcript 内容，状态应指向 Session owner。

## 验证

- 全仓 conversation 词项复核，保留项按 transcript/model context、自然语言或 provider protocol 归类。
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮只改命名说明和一条工具用途文本。

## 回滚与重置

恢复注释与工具说明文本即可回滚。没有配置、持久化或 wire shape 变化，无需重置。
