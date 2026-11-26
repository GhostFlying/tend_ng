# 具名 Agent 与任务分配

**一期范围。** 具名 Agent 有稳定的选择身份和实际执行配置，核心字段是 harness、模型和 reasoning effort，可附简短的用途或成本说明。首期使用 Codex App Server。Server 在本机配置并执行 Router，Runner 配置并执行 Issue Agent、Work Agent 和 Review Agent。支持 `image_gen` 作为明确的任务能力要求。

## 默认按要求派发，必要时指名

**已确认。** 主 Chat 的 Router 使用独立的 Server 配置，显示名可选，不参与 Runner 候选匹配。Router 按原交付选择顶层 category。单人可交付时用 `implementation` 或 `investigation`，范围小、路径明确的实现可由派发方显式指定 `implementation-economy`。多个独立交付需要协调时用 `issue-router`。派发和待安排记录都可指定 category，省略时（含旧记录）仍按 `issue-router` 解释。实际工作子 Issue 只在该顶层 Issue 所属 Runner 上匹配。派发方可以查询合格候选并指名。按要求派发和指名派发共用匹配逻辑。 [Issue 树的 Runner 归属](server-runner.md#issue-树的-runner-归属)

1. **Runner 上报具体配置。** 本机保存执行与访问配置，Server 从 Runner 上报构建清单。顶层 Issue 首次选择时，多台 Runner 提供的合格配置可以共同参与匹配。同名配置有差异时按 Runner 和配置区分。
2. **派发方提交任务要求。** 顶层 Issue 的派发携带目标、约束及已知的工作要求。实际使用 repo 或既有 workspace 的子 Issue 填写相应条件。用 `category` 表达用途和配置的默认选择，用 `required_capabilities` 提出实际必需的能力。用户明确指名时保留原选择及其来源。
3. **Server 分配。** 在允许调度的 Runner 上，核对角色对应的 category、实际派发所需的 repo 或 workspace、必需能力、具名选择及可用容量。选定 Runner 后，按配置的默认顺序选择合格的 Agent，或使用指名的 Agent。Server 持久记录实际分配与配置版本。Runner 接收时核对本机条件，无法满足时保留具体原因。

候选查询可以按这项任务筛选，只提供会影响选择的名字、用途、模型、effort、能力及可用状态。派发方通过本机工具读取最近同步的清单，Server 落实分配时核对当前事实。执行端也可通过工具本地受理按要求派发，由 Server 应用时选择执行配置。Agent 的操作范围按实际角色和已领取的 Issue 校验。

已接收的任务在本地保存所选配置版本、实际启动参数和可核对的执行身份。对账和恢复沿这次执行的记录查证。新 Issue 按目标选择 Agent，已有 Issue 持续沿用当前 Owner，换人走 Issue 交接。Session 的复用是成本优化，Issue 的责任和工作事实由持久记录接续。

## 顶层 Issue 首次选 Runner

**已确认。** Router 在顶层派发时带上已经明确必需的 repo 和 capability。Server 筛选能访问全部所需 repo、支持所选顶层 category 的 Runner。直接 Owner 的必需 capability 匹配实际工作的具名配置。对 `issue-router`，则核对同一 Runner 是否有满足工作能力的 Work Agent。`image_gen` 等工作能力按该 Runner 上符合用途的具名 Agent 配置核对。例如工作需要 repo X 和 Y，Runner B 能同时访问两仓并有合适的配置，就可以成为候选。

对于未显式指定 Runner、没有前序 Task 或既有 Runner 归属的顶层 Issue 首次派发，Server 在满足全部资格的候选中选占用比例最低的 Runner。占用数等于最新上报的 `slot_limit - available_for_assignment`，再加上 Server 尚未收到领取结论的 `reserved` 和 `issued` 分配数。分母通常为 `slot_limit`，接替期间使用 Server 当前授予后继的执行上限。占用数达到执行上限时，该 Runner 不参与首次派发。

比例相同时，优先选择最久未获此类自动预分配的 Runner。没有这类记录时优先，仍并列才按 Runner ID。轮换顺序与预分配同一事务持久化，跨 Server 重启保留。显式指定 Runner、子 Issue、已绑定接续和交接不参与轮换，继续沿原选择和归属调度。

尚未明确的要求在具体子 Issue 派发时核对。领取后新增工作能力要求时，仍在所属 Runner 上匹配。缺少合格工作配置时，保留原派发的待分配状态，并让派发方看到缺少的能力。 [绑定后新增能力的 TODO](design-questions.md#todo-绑定后缺少必需能力)

## Category 与 capability

一期使用以下五个 category。

| Category | 角色 | 派发场景 |
| --- | --- | --- |
| `issue-router` | Issue Agent | 顶层 Issue 的委派、跟进、汇总和结案。 |
| `implementation` | Work Agent | 常规实现，包括方案尚需探索、涉及多组件或需要较多技术判断的工作。 |
| `implementation-economy` | Work Agent | 范围小、路径明确、验收直接的实现，由派发方结合这项工作主动选择。 |
| `investigation` | Work Agent | 独立交付调查结论、复现结果或证据。 |
| `review` | Review Agent | 通过子 Issue 委派独立审阅，交付指定候选和范围的审阅结果。 |

`investigation` 和 `review` 按实际需要派发。缺少合格候选时，Server 保留原派发要求，并在分配结果中提供待分配原因。派发方决定是否改选。模型和 effort 只取自实际具名配置，顶层或子项角色不覆盖该配置。

旧 Tend 用跨 Runner 的 category 表达工作分类，Runner 本机定义具名 Agent、runtime、模型和 effort。后来普通派发用 `category_id` 和 `required_capabilities` 说明要求，由调度器解析为具体执行配置。共享 PR 审阅出现过真实执行已选出 Agent、报告也已持久保存，登记入口仍因创建时缺少 `spec.agent_id` 无法使用的回归。修复沿实际执行的冻结配置登记原报告。 [旧 Agent 定义](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/agent-definitions.md#L3-L25) · [旧工作要求](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/task-work-requirements.md#L3-L21) · [审阅身份绑定回归](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/category-shared-review-binding.md#L3-L14)

任务可以叠加必需 capability，例如用 `image_gen` 请求原生生图。Codex 源码会根据配置、模型和 provider 条件决定工具可用性。Runner 按具体 App Server 配置上报，接收时核对所选版本和实际能力。 [Codex 的工具可用性判定](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/core/src/tools/spec_plan.rs#L800-L825)

例如某项修改需要在 repo X 里加入一张用 `image_gen` 生成的图。Issue 树已绑定 A，如果只有 B 的 Agent 有 `image_gen`，该子 Issue 等待 A 的配置满足要求，或由派发方决定如何调整。候选查询、自动分配和指名提交应用同一组条件。结果、回执和后续业务处理沿实际执行配置查证。

## 无候选时的反馈与改选

**已确认。** Server 持久记录原任务的待分配状态和具体原因，关联原派发、当前工作要求及可核对的候选事实。派发方可以读取结果。需要派发方判断的变化异步送回原工作上下文，离线时保留待投递记录。可用性报告过期时，沿最近证据标出待确认的条件。旧 Tend 的同一条无候选消息可来自 category、指定 Agent 或容量等多种条件，新设计保留真正影响选择的原因。 [旧候选路径](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/scheduler/agent_execution.go#L77-L86)

首次匹配在 Server 应用派发后用当前事实进行。后续心跳可更新候选和交付进度，Server 自己处理未交付预分配的到期。 [匹配时机与预分配有效期](server-runner.md#匹配时机与预分配有效期)

| 情形 | 建议处理 |
| --- | --- |
| **有合格配置，暂时忙或离线** | 原任务保留在待分配状态。已绑定 Runner 的任务等该 Runner 恢复可用，再按原要求匹配。派发方按任务优先级和时限判断是否需要调整或汇报。 |
| **当前组合缺条件，派发方可以调整** | 给派发方一次需要判断的反馈，标明缺少的 repo、cap 或指定配置。派发方可在原目标和授权内改选自己决定的 Agent，或调整工作拆分以满足要求。 |
| **需要用户选择或操作** | Router 在原 Chat 发出 Attention，说明卡住的工作、已知原因、可选动作及影响。例如需要用户注册仓库，或同意变更其明确指定的 Agent。 |

Server 对同一任务的同一等待原因合并反馈。心跳、普通排队和重复同步只更新实际观察。影响选择的新事实才通知原派发方。旧 Tend 的观测唤醒复盘可作为验证样本。 [旧 PR 观测唤醒复盘](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/pr-external-waits.md#L5-L14)

改选沿原任务提交，Server 核对当前工作要求、原派发状态和所属 Runner。确认原派发尚未送达或已拒收时可以应用改选。原接收或执行结果未知时沿原身份查证。用户明确的条件依其授权变化。若 repo X 的配图能由同一 Runner 上另一具名 Agent 独立生成，负责集成的 Work Agent 可将生成素材委派给子 Issue。

## 定向验收提案

用两个 Runner 模拟协调配置、repo、`image_gen` 和容量分散的情况。首次派发带有明确的工作要求时，核对顶层 Issue 只匹配同时具备所需 repo 和对应工作配置的 Runner。首次领取后才新增必需能力时，核对原 Runner 缺少配置的原因回到派发方，即使另一台 Runner 有该能力也沿原 Runner 等待。用同一组事实验证子 Issue 指名提交。若 Runner 在取件前撤下所选配置版本，原任务保留配置引用及实际不能执行的原因。用真实 App Server 定向验证标记 `image_gen` 的配置可以生成并回交产物。

用同一 fixture 验证三条候选恢复路径。暂忙后恢复可用时分配原任务。缺少能力时多次心跳只向原派发方反馈一次。改选与原任务送达并发时，以实际接收或拒收事实结算，用户明确指定的 Agent 保持原选择，等待其授权变更。
