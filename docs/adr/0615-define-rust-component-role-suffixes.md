# ADR 0615：明确 Rust 组件角色后缀

## 状态

已采纳并实施。

## 背景

多个 crate 都使用 `Engine`、`Runtime`、`Store`、`Registry`、`Manager`、`Service` 与 `Facade` 后缀，但命名规范此前没有集中定义各自指向的主要 owner。扫描确认这些后缀对应不同的职责边界：例如 `McpManager` 管理 MCP 连接生命周期，`SkillRegistry` 提供已发现技能的索引，`ToolRunService` 协调 ToolRun 执行与终态，`ApplicationRuntime` 持有应用进程的长生命周期资源，`ToolsFacade` 组合工具入口。

## 决定

1. 在 `docs/naming.md` 记录 Rust 类型角色后缀及仓库实例：
   - `Engine`：领域算法或执行循环。
   - `Runtime`：进程内活动状态、资源和生命周期。
   - `Store`：持久数据访问或 typed persistence boundary。
   - `Registry`：按 identity 索引的目录、catalog 或规则集合。
   - `Manager`：外部或子系统资源的创建、替换、重连和关闭生命周期。
   - `Service`：组合多个 owner 完成的应用/领域用例。
   - `Facade`：组合窄接口提供统一入口，但不夺取被组合 owner 的生命周期。
2. 后缀表示类型的主要责任，不要求某个类型只拥有一种次要行为；若类型同时承担多个主要 owner，路线图审计应检查能否拆分或改名。
3. 这些规则指导全仓逐项审计；本 ADR 不批量重命名类型，也不把某个后缀机械套用到所有类。

## 替代方案

- 所有状态容器统一叫 `Manager` 或 `Service`：拒绝，它会抹掉应用用例、索引目录、资源生命周期与持久访问的差别。
- 只按类型所在 crate 选择后缀：拒绝，同一 crate 可同时含引擎、store、registry 与 facade。
- 本次批量重命名所有现存类型：拒绝，需按调用边界核对每个类型的实际 owner；含糊或多责任实例通过路线图继续处理。

## 影响与验证

- 只增加命名规则与审计基准，不改变代码、wire、配置、持久化或运行行为。
- 命名路线图 §5.7 继续保持 Active；后续继续核对每个实例是否符合其后缀以及是否存在角色重叠。
- 验证：ADR 索引、交叉链接及文档 diff 检查。

## 回滚

删除后缀角色表并将路线图恢复为未分类状态；无代码或数据迁移。
