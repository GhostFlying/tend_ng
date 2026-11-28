# Tend NG 工程约定

## 开工前

- 先读 [总体设计](docs/design.md) 和 [工程结构](docs/engineering.md)，再按改动范围查阅对应契约。
- 编码前在 `docs/plans/` 记录本次目标、边界和验证方式。首段实现及一期验收见 [首个产品切片](docs/plans/first-product-slice.md)。
- 用户确认新的设计决定后，更新对应的专题契约。影响所有工作 Agent 的约束也更新到本文件。讨论中的设计作为本地草稿。 [设计决策与调研记录](docs/design-questions.md)

| 改动范围 | 对应契约 |
| --- | --- |
| 主 Chat、Issue Chat、Attention、通知、Queue 与 Steer | [Chat 路由与结果回到原对话](docs/chat-routing.md) |
| Issue、Task、Run、审阅、取消、等待与续跑 | [任务生命周期](docs/task-lifecycle.md) |
| 具名 Agent、category、capability、候选匹配 | [具名 Agent 与任务分配](docs/agent-selection.md) |
| 派发、Runner 生命周期、workspace、同步与升级 | [Server 与 Runner 的契约](docs/server-runner.md) |
| 工具调用、操作回执、session 恢复 | [Agent 工具与本地受理](docs/agent-tools.md) · [Codex 原生会话](docs/codex-rollouts.md) |
| PR / CI 关联、采集与投递 | [外部事实与等待](docs/design.md#4-消息投递外部事件与等待) |
| SQLite、消息与交付原件、查询代价 | [数据库存储与访问边界](docs/storage.md) |

## 设计约束

- 为责任 Agent 提供所需的工具、事实和权限。角色 Skill 和预置 prompt 指引计划、拆分、验证、审阅、重试和收尾，Agent 根据任务及仓库要求作决定。程序保障身份、授权、独占、执行事实、持久投递和副作用对账，按实际问题增补必要的限制。 [Empower Agent](docs/design.md#empower-agent)
- 按可信单用户部署。故障处理和测试覆盖正常运维中可发生的断线、进程重启、响应丢失等场景。Issue 及其持久关联是工作事实来源，session 可从原输入、Issue 事实和原操作回执恢复。 [工作记录](docs/design.md#工作记录) · [Session 接续](docs/agent-tools.md#session-接续)
- 主 Chat 由 Server 本机的 Router 代理。单人可交付的顶层 Issue 由 Work Agent 直接领取，多个独立交付需协调时由 Issue Agent 委派。Owner 在 Runner 持久领取后生效，显式决定自己的 Issue 何时结案。交付与需用户决策的事项沿责任链回到 Router，再回到原 Chat。CLI 覆盖全部公开能力，与 Web 共用 API。 [产品与交互](docs/design.md#1-产品与交互) · [Agent 角色](docs/design.md#agent-角色)
- 首次待安排 Issue 保存意图，尚未创建执行 Task。已开始的开放 Issue 也可原地退回待安排，保留历史且仅显式开工恢复调度。看板以顶层 Issue 树的事实分列，有实际 Run 时显示进行中，有可安排工作或可处理输入时显示待推进，整棵树只等已登记的条件或回复时显示等待中。已完成与已取消沿业务终态展示。人工检查通过原 Chat 的 Attention 处理，卡片显示待你决定。 [看板阶段](docs/plans/issue-board-phases.md)
- Server 和 Runner 各自运行和持久化。已领取任务可以离线推进，按原 Task、Run 和操作身份全量对账或增量同步。工具写操作先在执行实例持久受理。业务状态、操作结果与待投递记录在所属实例的同一短事务提交，响应丢失时沿原身份查证。 [同步 API](docs/server-runner.md#一期同步-api) · [写入与故障边界](docs/storage.md#写入与故障边界)
- 部署可热更新 Runner 的分类与具名 Agent。新 Task 依据当时有效的目录及执行配置受理；已绑定的 Task 在排队、重试、复审和同 Host 接替时保留原具名 Agent、CLI、模型及 effort。 [部署分类目录](docs/agent-selection.md#部署分类目录)
- 按实际停妥证据归还 slot。原 Task 在 `waitfor` 期间继续独占 workspace。绑定审阅只在作者 Run 实际停妥后复用原目录，审阅 Run 停妥后作者才续跑。每一层的 `waitfor` 命中后，经负责原 Task 接续的 Runner 直接送给登记等待的 Agent。同 Host 接替时，前驱负责旧 Run 停妥，后继取得接续权后可在原 workspace 启动下一次 Run。同 Host 保留原件和原 Codex home；前驱私有数据的清理以 Server 最终同步确认、实际退出和已验证的共享原件与历史读取绑定为前提。 [等待与续跑](docs/task-lifecycle.md#命中事件与续跑) · [同机 Run 交接](docs/server-runner.md#同机接替时的-run-交接)
- Runner 常规升级默认使用 main-build 的 `bin/tend-runner-upgrade` 同机接替。两代共用 Server 持久记录的前驱原 slot 预算，前驱自然停妥并实际退出后再退役。用同一 Runner ID 和库重启只用于本机恢复。 [常规升级](README.md#runner-常规升级默认同机新旧代交接)

- GitHub PR / CI 由 Server 使用自己的只读权限采集。Runner 管理本机仓库与 Git 访问。采集范围来自已关联或明确登记的对象。 [外部事实](docs/design.md#pr-和-ci-事实的采集与投递)

## 开发与验证

- Server、Runner 和 CLI 用独立 Rust 构建入口，tsnet 用独立 Go 进程。Server 和每个 Runner 各用自己的 SQLite WAL 库。开发和验证在 devcontainer 中完成，Web 工具链沿用旧 Tend 的基线。 [基础选型](docs/engineering.md#基础技术栈)
- 测试用固定输入和 App Server 输出 fixture。局部修改跑定向模块测试，共享接口变化验证受影响的边界。少量端到端测试用真实 Server、Runner 和临时 workspace，核对用户可读的结果、原身份、实际副作用和资源释放。首期以本地定向验证支撑迭代，CI 卡口在稳定后确定。 [实现顺序与验证范围](docs/engineering.md#实现顺序与验证范围)
- 所有 Tend 管理的 Codex 会话，包括隔离研判与转交，都在各自显式配置的独立 `CODEX_HOME` 保留原生 rollout，不启用 ephemeral，不回退个人 `~/.codex`。原件保留在所属实例和 home，不复制进仓库、业务库或 UI。 [原件及排障契约](docs/codex-rollouts.md)
- 真实 Codex 验收须显式指定独立、已认证的 Codex home，验收会话与个人会话分别存放。使用 `tests/e2e/live_codex.py --codex-home` 运行真实验收，日常 fixture 使用模拟 App Server。凭据保存在独立 home 的本机配置中。

## 提交与交付

- 保留用户约定的交付终点。要求合入时，满足仓库现有合入门禁、做定向验证并核对远端 main SHA 即可交付。仅当用户明确要求 CI 绿、出包或部署时延伸到该终点。观察由一位责任人负责，沿责任链共享精确 SHA 与结果，等待已有子项输入时结束回合。
- 当前阶段直接提交到 `main`，并直接推送到 `origin/main`。此约定适用于 Tend NG 仓库。Work Agent 按所操作仓库自己的约定提交。
- 提交使用仓库现有的用户 Git 身份，未设置仓库身份时沿用用户的全局配置。
- 提交标题和正文均使用英文。标题以 `feat: `、`fix: ` 或 `opt: ` 开头，整行小写。正文仅详细说明变更范围、背景、原因及影响。测试执行情况单独记录在交付说明。
- UI 变更交付时附上实际页面的最终渲染图，有设计图时一并提供。用户要求先出设计图再改动时，完成设计图后再实施，并将该设计图纳入提交。
- 版本管理包含设计、源代码、脱敏 fixture、依赖清单和锁文件。凭据、私有会话、本机路径、原始运行数据和构建缓存保存在本机配置或数据目录。
