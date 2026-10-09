# ADR 0857：集中规范实体 ID 校验

## 状态

Accepted — 2026-10-10

## 背景

`haven-tools::builtin::messaging` 与 `haven-messaging::messaging_service` 各有一份 `is_canonical_message_id`。两者逐字实现相同的检查：`msg-` 后恰有 32 位小写十六进制字符。它们分别在工具参数及消息/回执/ack 输入边界使用；若 ID 格式规则变化，两处必须同步，否则同一 ID 会在不同层被不同接受。

实体 ID 的格式和前缀表已由项目规范集中定义，`haven-common::types::new_id` 是生成 owner；尚无相应的格式校验 owner。

## 决定

- 在 `haven-common::types` 增加 `is_canonical_id(id, prefix)`，其中 `prefix` 不含分隔连字符；该函数验证前缀是小写字母/数字，并验证后缀为恰好 32 位小写 hex。
- Tools 与 Messaging 的 `msg-*` 入口都调用 `is_canonical_id(id, "msg")`，删除各自的私有校验函数。
- 在 AGENTS 与命名规范中记录：规范 ID 由 Common 生成和验证；调用者继续拥有针对具体输入字段的错误分类、消息和拒绝时机。
- 不改变已有前缀表、ID 值、消息协议、数据库字段或错误文案。

## 替代方案

- 保留每个领域独立校验：拒绝。相同 UUID32 格式已形成重复规则 owner，消息 ID 在 Tools 与 Messaging 的接受条件必须一致。
- 为每种实体新增 ID newtype：拒绝。当前调用边界接收的是 wire 字符串，规范已要求仅在确有类型隔离需求时创建 newtype；通用格式 validator 足以复用不变量。
- 在 Common 维护所有业务字段的完整 validator：拒绝。Common 只验证给定前缀的格式；消息类型、字段是否必填及错误文案仍归 Messaging/Tools。

## 影响与验证

这是 Common 跨 crate 的纯验证 helper 及两个调用点调整；无 IPC、事件、配置或持久化 schema 变化，无需数据库/配置重置。两侧既有无效/有效消息 ID 行为测试保留；本轮按执行约束未运行测试套件。

验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、crate dependency guard、ADR index、
ADR Prettier 与 `git diff --check`。未运行测试套件。

## 回滚

若调用者需要不兼容的不同 ID 格式，应为差异提供明确的域规则；否则整体撤回公共 validator 与两个调用点，并恢复各自实现。不新增旧函数别名。无持久数据需要重置。
