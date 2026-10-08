# ADR 0763：导航组件共享 tabs selection props

## 状态

已采纳并实施。

## 背景

`WorkspaceNav`、`LandscapeWorkspaceNav`、`MaterialTabs` 和 `AppShell` 都接收 `NavigationTab[]`、活动 tab id 和导航回调；前三项入口曾各自声明同形字段，并将 ID 独立写成 `string`。四个消费者表达同一 navigation selection handoff，字段 owner 与 `NavigationTab` 分散。

## 决定

- 在 `navigationTypes.ts` 新增 `NavigationTabsProps`，统一拥有可选 `tabs`、`activeTab` 与 `onNavigate`。
- `activeTab` 和 callback 参数都引用 `NavigationTab['id']`，字段类型随 tab identity owner 派生。
- `WorkspaceNav` 与 `LandscapeWorkspaceNav` 直接消费共享 props；`MaterialTabs` 和 `AppShell` 扩展共享 props 后组合各自设置与 shell 输入。
- 保持动态 tab ID；Memory、Settings 与 Tools 的页面路由仍由各自 source tuple 局部收窄。

## 影响与验证

只合并 UI 内部 props owner，不改变导航顺序、tab ID、callback 行为或路由状态。Landscape navigation test 不再通过 `as any` 绕过组件 props。固定 Node 24.20.0 下的 UI check 与完整 test suite 通过。

## 回滚

将共享 interface 拆回各组件本地声明即可；无需 wire、数据库或配置迁移。
