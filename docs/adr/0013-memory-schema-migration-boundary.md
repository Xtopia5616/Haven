# ADR 0013：Memory 当前 Schema 与历史迁移边界

## 背景

P2 的 Memory 目标要求分离 schema、图谱写入、查询/排序、嵌入和迁移策略。
在此前实现中，crates/memory/src/schema.rs 同时包含当前表结构、11 个历史
迁移、user_version 编排、FTS/embedding 修复以及对应测试，导致当前数据库
形状与历史升级策略难以分别审查。

## 决定

- 新增 crates/memory/src/migrations.rs，由它唯一拥有历史迁移目录
  (MIGRATIONS)、版本戳读写、迁移执行器和迁移专用表/列探测。
- schema.rs 只拥有当前幂等 schema、FTS/embedding 维护对象、必需列检查和
  初始化编排；通过受限的父模块接口调用迁移目录。
- 保持 SCHEMA_VERSION = 11、每个迁移的顺序和逐步写入 PRAGMA user_version
  的崩溃可重试语义不变。ReActSnapshot.events、投影表和 Memory 仓库写入
  契约不变。
- 迁移仍是纯数据库边界：不引入 Agent 推理、UI 展示或 provider 逻辑，也不
  增加数据库/配置兼容层。

## 替代方案

- 继续在 schema.rs 中增加迁移：改动最小，但会继续扩大当前 schema 与历史
  迁移的职责混合，拒绝。
- 把迁移放入各 repository：会让仓库层承担 schema 生命周期，并可能按业务
  查询路径触发升级，拒绝。
- 修改数据库版本或重写迁移：本次仅做结构重构，没有必要破坏现有数据契约，
  拒绝。

## 影响

这是内部模块拆分，不改变现有数据库文件、schema 版本、表名、字段、ID 规则
或对外 API。没有数据库、配置或缓存重置要求；旧数据库仍按原有迁移链升级，
损坏或过旧数据库仍按 docs/release-and-reset.md 的重置说明处理。

## 验证

    cargo fmt --all -- --check
    cargo test -p haven-memory --lib -- --test-threads=1
    cargo check --workspace
    cargo clippy --workspace -- -D warnings
    cargo test --workspace -- --test-threads=1

迁移模块新增目录顺序/终版本回归测试，既有 220 个 Memory 单元测试继续覆盖
schema 幂等、版本拒绝、各历史迁移、FTS 和 embedding 触发器。

## 回滚与重置

代码回滚时恢复 schema.rs 中的迁移实现即可；由于本次不改变迁移 SQL、版本
号或数据库内容，不需要数据库或配置重置。
