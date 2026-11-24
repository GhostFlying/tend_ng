# Tend NG 设计决策与调研记录

本页按总体设计的顺序记录旧 Tend 的行为、代表性故障及 Tend NG 的决策依据。旧实现以 `83bc15ae` 的源码和记录为基线。实现和验收以总体设计及其链接的专题契约为准。 [总体设计](design.md) · [首个产品切片](plans/first-product-slice.md)

| 层 | 旧 Tend 的做法和故障 | Tend NG 的设计约定 |
| --- | --- | --- |
| 1. **产品与交互** | 主 Agent 负责调度，Issue 有直接对话。同一 Attention 在 Chat、Inbox 和详情展示。旧版后来收紧了普通阶段进展进 Chat 的规则。 [主 Agent 职责](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/design.md#L105) · [Issue 对话与 Attention](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/issue-agent-design.md#L63-L75) · [结果发布](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/quiet-chat-publication.md#L3-L12) | 主 Chat 的对话由 Router 代理。各顶层 Issue 的结果分别上报 Router，由它按约定分开发送或重新组织。用户主动在 Issue Chat 发言时与对应 Owner 直接对话。看板、通知和父子 Issue 合并展示留言的定位已定，执行过程收起并按需加载。 [结果怎样回来](chat-routing.md#结果怎样回来) · [Issue Chat 合并展示](chat-routing.md#issue-chat-合并展示) · [Issue Chat 的留言与执行过程](chat-routing.md#issue-chat-的留言与执行过程) |
| 2. **工作对象与结算** | Task 记录执行结果，Issue Owner 控制关闭。#336 将普通工作改为 Agent 自主选择审阅。#409 修复已停妥的失败执行缺少报告时无法收尾。#412 提出给同一 Task 的多笔交付单独保留身份。 [自主执行 #336](https://github.com/GhostFlying/tend/pull/336) · [失败执行结算 #409](https://github.com/GhostFlying/tend/pull/409) · [后续交付提案](https://github.com/GhostFlying/tend/pull/412) | Issue、Task、Run 的边界、Task 结算、失败接手和独立审阅入口已定。Task 可无产物结算，Runner 可依据正常停妥的 Run 补记完成。需要决定的事项逐级上报，取消和重开后的状态已定。 [工作记录](design.md#工作记录) · [Task 结算](task-lifecycle.md#run-正常结束时的-task-结算) · [执行失败后的接手](task-lifecycle.md#执行失败后的接手) · [取消与逐级停止](task-lifecycle.md#取消与逐级停止) · [重开后的接续](task-lifecycle.md#重开后的接续) |
| 3. **任务分配、执行与工作区** | 旧 Issue Manager 使用独立 category，复用 Runner 的领取和回执。普通工作按 category 和 capability 选具名配置，已领取任务后来获得离线执行能力。 [Issue Manager 契约](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/runner-issue-managers.md#L5-L17) · [任务派发](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/task-work-requirements.md#L3-L21) · [仓库登记](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/online-repositories.md#L7-L13) · [离线执行 #391](https://github.com/GhostFlying/tend/pull/391) | 一期五个 category、即时匹配、Runner 取件、slot 运行口径、Repo 本机注册、首次选 Runner 和整棵树的 Runner 归属已定。顶层默认共用 workspace，`waitfor` 期间保留独占，结案后保留现场，回收由 CLI 显式触发。 [Runner 归属](server-runner.md#issue-树的-runner-归属) · [Workspace 分配](server-runner.md#workspace-分配) · [Workspace 回收](#workspace-的保留和回收) · [绑定后缺少必需能力](#todo-绑定后缺少必需能力) |
| 4. **消息投递、外部事件与等待** | 对活跃执行的补充绑定实际回合，接收和消费分别记录。#406 用精确 PR 候选及运行身份匹配等待。 [运行补充消息](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/issue-agent-design.md#L63-L67) · [精确订阅与旧唤醒重放 #406](https://github.com/GhostFlying/tend/pull/406) | Queue、Steer、`waitfor`、等待期间用户输入的及时唤醒、Server 采集 PR / CI 的范围与收件规则、PR 关联的两个入口及工作 Task 收尾后的单次自动补查已定。 [外部事实采集](#pr-和-ci-事实的采集与投递) · [PR 与 Issue 关联](#pr-与-issue-关联) |
| 5. **同步、持久化和恢复** | 消息页和空领取曾放大读写。旧版丢 ACK、短断联和停妥后各有恢复入口。归档及交付锁曾牵连主链，交付和历史对账争用同一把锁。 [消息页读放大](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/web-messaging-read-amplification.md#L3-L10) · [空领取对照](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/findings/issue-lifecycle-efficiency-performance.md#L27-L43) · [回执恢复](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/manager-run-timeout-recovery.md#L3-L12) · [归档隔离 #306](https://github.com/GhostFlying/tend/pull/306) · [原件回传 #390](https://github.com/GhostFlying/tend/pull/390) · [交付持锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/mainthread.go#L1255-L1267) · [历史对账取锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/turn_receipt.go#L222-L244) | 全量和增量的职责、短事务和分页读取已定。工作 Agent 的留言和交付文件同步到 Server，rollout 由实际执行实例保管并按需读取。Runner 首期保留可通过全量对账补回的交付副本。Server 和每个 Runner 一期各自使用本地 SQLite WAL，用组合负载验证。覆盖确认的协议细节随实现展开。 [全量对账与回执](#全量对账与回执) · [选型和验收](storage.md#选型和验收) · [按需读取 rollout](server-runner.md#按需读取-rollout) |
| 6. **工程结构与首期验收** | 后期按真实业务入口验证交付和恢复，用故障 fixture 核对影响范围。 [业务主链测试约束](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/business-flow-contracts.md#L20-L32) · [主链故障测试 #307](https://github.com/GhostFlying/tend/pull/307) | 一期采用 Rust 业务进程和外置 Go tsnet，技术 smoke 已通过。devcontainer、旧版 Web 工具链、定向测试、Router 托管、独立升级的通道和并行接替后的 Issue 归属已定。第一次试用以 Chat 交付为完成线，Attention 上报和答复为一期必验。写操作沿原 ID 查证，Router 沿原输入和回执恢复。Agent 用 Skill 和本机注入工具，按原生回调身份本地持久受理。 [工作 Runner 离线时的主 Chat](#工作-runner-离线时的主-chat) · [独立升级](#server-与-runner-的独立升级) · [并行接替的 Issue 归属](#并行接替的-issue-归属) · [首个产品切片](plans/first-product-slice.md) · [Router 输入的恢复边界](chat-routing.md#router-输入的恢复边界) · [Codex 注入 Tend 工具](agent-tools.md#codex-注入-tend-工具) |

## 实现起点

第一次试用的用户行为、实现入口和验收已经确定。第一段从空库跑通 CLI 发消息、Server 持久收件和 Router 在原话题回复，再接一个 Runner 的分发与交付。Attention 的逐级上报和答复按已定范围进入一期。 [第一段实现](plans/first-product-slice.md#第一段实现) · [一期 Attention 验收](plans/first-product-slice.md#一期-attention-验收)

首次试用只用一个 Runner。同机并行接替采用 Run 边界交接，已启动的 Run 由前驱收尾，后继在原 Host 上接续。同步间隔、批次大小、具体 API 字段和 SQLite 表结构随对应模块实现确定。 [同机 Run 交接](server-runner.md#同机接替时的-run-交接) · [Run 边界更换 Runner 的调研](#run-边界更换-runner-的调研)

## 顶层 Issue 首次选 Runner

旧 Tend 在 Issue Manager 首次领取时确定协调会话的 Runner。普通任务另按自己的 repo 和已有 workspace 主机匹配。 [旧 Issue Manager 领取](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/runner-issue-managers.md#L5-L11) · [旧任务的 repo 与 workspace 匹配](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/scheduler/scheduler.go#L164-L185)

**已确认。** 顶层首次派发用已经明确必需的 repo 和工作能力筛选 Runner，领取时确定整棵 Issue 树的归属。 [首次选 Runner 的条件](agent-selection.md#顶层-issue-首次选-runner)

## TODO 绑定后缺少必需能力

Issue 推进中可能新增必需 capability，且原 Runner 无法提供。出现实际需求后，评估是显式迁移整棵 Issue 树，还是只把需要该能力的子 Issue 派到另一台 Runner。一期保留该子项的待分配状态及缺失能力，由派发方判断能否补齐本机配置或调整工作。 [所属 Runner 的匹配规则](agent-selection.md#顶层-issue-首次选-runner)

## 共享 worktree 的占用

旧 Tend 的 workspace 由 Task 占用，暂停和待决期间继续保留占用，Task 终止且执行停妥后释放。 [旧 workspace 占用](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/workspace-reuse-and-deliverables.md#L12-L16)

**已确认。** `waitfor` 在执行停妥后归还 slot。原 Task 在等待及待续跑期间继续独占工作目录，同一目录的其他 Task 等原占用释放。 [Workspace 分配](server-runner.md#workspace-分配)

## Workspace 的保留和回收

旧 Tend 的到期回收例程处理释放至少七天且无活动绑定或执行的普通目录，Git worktree 保留在原处。 [旧到期回收条件](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/workspace_lifecycle.go#L122-L128)

**已确认。** 一期在顶层 Issue 结案后保留 workspace，由 CLI 显式触发回收。Runner 按实际目录占用和停妥证据核对回收条件。 [Workspace 保留与回收](server-runner.md#workspace-保留与回收)

## 等待期间的用户输入

旧 Tend 由 Issue Manager 处理 Issue 直接聊天和目标变化，发给执行中的 Worker 的补充绑定原 Run 和回合。 [旧版 Issue 对话与执行补充](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/issue-agent-design.md#L63-L67)

**已确认。** `waitfor` 期间的用户消息，包括普通补充和 Attention 答复，及时唤醒对应 Owner。Owner 在原 Task 中处理新输入，同时可查原等待条件及当前证据。 [等待期间的用户输入](task-lifecycle.md#等待期间的用户输入)

## PR 和 CI 事实的采集与投递

旧 Tend 用独立于 Agent 执行的观察器查外部状态，Runner 和 Server 都有采集入口。常规观测曾反复唤醒 Agent，后来把符合具体等待条件的结果与普通观测区分处理。 [旧交付观察器](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/delivery-observer.md#L3-L9) · [旧 Server 观察器配置](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/serverkit/delivery_observer.go#L18-L25) · [普通观测唤醒修复](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/pr-external-waits.md#L3-L10)

**已确认。** 一期由 Server 采集 PR / CI。已明确 Issue 归属的默认投递经所属 Runner 交给顶层 Issue Agent，归属未定的输入进入 Server 本地 Router 队列。每一层的 `waitfor` 命中都经当前负责原 Task 接续的 Runner 直接交给登记等待的 Agent。 [PR 和 CI 事实的采集与投递](design.md#pr-和-ci-事实的采集与投递)

**已确认。** Issue 与 PR 的关联本身是观察入口。Server 观察 Tend 工作中明确关联或主动登记的对象。看板卡片直接显示关联 PR 的链接和已保存的状态。 [看板读取边界](storage.md#读取边界)

## PR 与 Issue 关联

旧 Tend 允许 Agent 或用户用 PR URL 关联 Issue。漏登记时，CLI 可以指定原 Task 和挂载，按已核实的远端仓库及初次记录的工作分支查询，匹配唯一 PR 才登记。两条入口使用同一个关联操作。看板按当前页批量读取关联和已保存的状态。 [旧 CLI 关联入口](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/cli/issues.go#L218-L234) · [旧分支补查](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/cli/issue_pull_requests.go#L20-L47) · [旧看板读取](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/views/issue_board_pull_requests.go#L21-L43)

**已确认。** 一期保留直接登记 PR URL 和按已记录的工作分支补查两个入口。Server 确认唯一补查结果后沿同一关联操作登记。 [PR 与 Issue 关联](design.md#pr-与-issue-关联)

旧 Tend 在实现 Run 已有完成结果后确认停妥，再采集 Git 状态并提交产物。 [旧 Run 收尾](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/execute.go#L846-L854) · [旧 Git 状态采集](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/execute.go#L885-L925)

**已确认。** 新版在工作 Task 到达终态且实际停妥后，复用分支查询做一次自动补查。后续补查由用户或 Agent 显式发起。 [首期补查约定](design.md#pr-与-issue-关联)

## 重开后的接续

旧 Tend 通过显式重开恢复同一 Issue 的责任，原 Task 和 Run 状态保持原样。重开的 Issue 从 backlog 开始，后续实施由责任 Agent 安排。 [重开的状态语义](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/domain/issue_controls.go#L128-L135) · [旧版重开入口](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/canonical_owner_lifecycle.go#L165-L177)

**已确认。** 旧子执行停止待确认时，父 Issue 可以先重开。已有 Task 和停止控制沿原执行处理，后续工作在原 Host 按 Issue 树当前归属调度。 [重开后的接续](task-lifecycle.md#重开后的接续)

## 取消的范围与状态

旧 Tend 取消一个 Issue 时，会先提交这个 Issue 直接所属 Task 的取消。它把未结子项、在途执行和待处理控制纳入取消结束检查，存在这些事实时 Issue 保持取消中。 [旧版取消入口](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/canonical_owner_lifecycle.go#L153-L164) · [取消结束检查](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/domain/issue_controls.go#L101-L116)

**已确认。** 整项取消预期沿委派关系逐级停止。停止控制发出后，父 Issue 可先记录业务已取消，子执行继续显示各自的实际状态。 [取消与逐级停止](task-lifecycle.md#取消与逐级停止)

## 执行失败后的接手

旧 Tend 的独立审阅曾在原生执行已经失败、进程已经退出时，因缺少完成报告而绕回传输恢复，导致原执行的 slot 未释放。#409 让 Runner 凭失败和实际停妥证据收尾，后续执行由负责人另行选择。回执丢失则沿原身份查证。 [失败执行结算](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/failed-worker-review-recovery.md#L3-L13) · [回执恢复](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/manager-run-timeout-recovery.md#L3-L7)

**已确认。** Runner 凭实际停妥证据在本机结束失败的 Run 并归还 slot。Owner 接手原失败事实，判断如何恢复或修复。执行系统中的非预期异常作为缺陷跟进，必要时逐级上报。 [执行失败后的接手](task-lifecycle.md#执行失败后的接手) · [逐级上报与用户决策](chat-routing.md#逐级上报与用户决策)

## 结案后的子 Issue 回传

旧 Tend 把尚未关闭或取消的直接子 Issue 纳入父 Issue 的结案阻塞。旧版通过显式重开恢复 Issue 责任，原 Task 和 Run 的历史状态保持原样。 [旧版子 Issue 阻塞收集](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/canonical_owner_lifecycle_facts.go#L13-L31) · [旧版结案与重开](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/domain/issue_controls.go#L119-L135)

**已确认。** 晚到事实和结果留在原子 Issue。业务状态改变、Agent 留言和新交付必达每一级上级，包括已经结案的 Issue Agent。重开由 Issue Agent 显式决定。父 Issue Chat 合并展示子 Issue 的原留言。 [等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒) · [Issue Chat 合并展示](chat-routing.md#issue-chat-合并展示)

普通留言的唤醒成本在一期试跑观察。固定输入回放已经比较共享 slot 持续占用和多次让出的情况，实际收益按触发来源和上级处理结果复核。 [留言唤醒回放](plans/child-issue-wake.md#回放结果)

## 独立审阅的后续规则

一期使用已确定的原生 subagent 审阅和 `review` 子 Issue 入口。 [独立审阅](task-lifecycle.md#独立审阅)

旧 Tend 的正式验收会在 `changes_required` 后按预算自动追加一轮实现。#336 把普通工作改为默认自行检查，责任 Agent 可发起或撤回可选审阅。共享 PR 审阅还有单独的报告登记流程，曾因创建时缺少 `spec.agent_id` 拒绝已由调度器选出 reviewer 并生成报告的任务。 [验收状态转换](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/domain/facts.go#L119-L137) · [自主执行 #336](https://github.com/GhostFlying/tend/pull/336) · [共享审阅登记故障](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/category-shared-review-binding.md#L3-L12)

旧 Tend #401 的独审在原生回合失败且进程已退出后缺少报告，恢复路径仍先查报告，原 Run 没有及时收尾。#409 将停妥后的失败结算从报告登记中解开。 [失败执行恢复说明](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/failed-worker-review-recovery.md#L3-L13) · [收尾修复 #409](https://github.com/GhostFlying/tend/pull/409)

**已确认。** 由 Tend 委派的审阅结果沿原委派回到发起审阅的 Issue Owner，并按已有父子 Issue 规则上报。例如，Work Agent 为自己的改动请独审，发现漏了断线重连用例，就由这个 Work Agent 直接处理。Server 和 Runner 两项工作整合后，若整体审阅发现先升级 Server 会让旧 Runner 无法同步，由发起审阅的顶层 Issue Agent 接收，再协调相应的 Work Agent 修复。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒)

审阅意见随所审候选、范围和证据保存。要求修改也是有效的审阅交付。发起方根据实际发现决定修复、记录已知风险、向上级报告，或为实质改动及争议发起定向复审。Work Agent 处理自身实现范围内的发现，顶层 Issue Agent 决定整体交付何时结案。用户、仓库或平台明确要求的审阅在相应操作上落实。

审阅 Task 和 Run 沿通用规则收尾。Run 正常结束且已停妥时，可以在零份交付的情况下结算 Task。审阅结论以实际登记的记录为准。执行失败时保留原错误，确认停妥后归还 slot。发起方依据实际结论或原执行结果决定是否再审。 [Task 结算](task-lifecycle.md#run-正常结束时的-task-结算) · [执行失败后的接手](task-lifecycle.md#执行失败后的接手)

## Issue、Task 与 Run 的结算

1. 旧 Tend 的 Worker 用 `submit_result` 提交执行成果。免独立验收的 Task 在 Server 应用成果时完成。有独立验收要求的 Task 等当前制品通过验收。 [结果工具](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/harness/result.go#L16-L20) · [Task 结算](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/domain/facts.go#L66-L91) · [验收结算](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/domain/facts.go#L113-L125)
2. Issue Owner 通过 `issues_close` 显式关闭，Server 核对未处理的义务和结果证据。用户回报按原 Chat 的交付约定发布。例如调查报告可以已经提交，Issue 继续承接后续修复。 [关闭工具](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/tools/hosted.go#L130-L134) · [Issue 关闭转换](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/domain/issue_controls.go#L119-L125) · [结果发布](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/quiet-chat-publication.md#L3-L8)

**已确认。** Work Agent 可单独结算 Task，也可随最终产物结算。Run 正常结束并实际停妥时，Runner 可以按本地记录补记原 Task 的结算，允许零份产物。Issue 由其 Owner 决定结案。Runner 按原 Run 保存实际执行及停妥证据，恢复原执行时沿原 Run，实际重新启动时创建新 Run。 [工作记录](design.md#工作记录) · [结算规则](task-lifecycle.md#run-正常结束时的-task-结算)

## 全量对账与回执

旧 Tend 协议 7 已能按 Run 异步补传完整原件，接收进度只跨过已连续收齐的记录，收到持久回执后清理传输副本。#393 的负责人事件则在业务处理成功后卡在 SQLite ACK 写入，原执行因此中止。#387 的回执恢复还遇到历史读取与实时交付争用同一把锁的问题。 [离线执行与异步回传](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/offline-runner-execution.md#L5-L13) · [连续接收范围](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/store/migrations/0141_received_event_sequence.sql#L1-L11) · [负责人 ACK 故障](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/manager-run-timeout-recovery.md#L1-L8) · [历史读取取锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/turn_receipt.go#L222-L244)

**已确认。** 全量对账按原 Task 及关联 Run 核对两端持有的工作记录，按原 ID 补齐缺项。接收端确认实际持久保存的记录，未收齐的范围保持可查。增量游标丢失时，可以从原记录重建。某个文件或回执待补时，无依赖的记录仍可保存和结算。分批大小与截止位置随同步 API 实现确定。 [一期同步 API](server-runner.md#一期同步-api)

旧 Tend 的退役 journal 曾反复在同一 Run 的上传上耗尽一轮维护时间，排在后面的 Run 得不到处理机会。修复后，下一轮从后续待处理 Run 继续，原超时记录保留待补。 [旧回传超时修复](https://github.com/GhostFlying/tend/commit/6cc3c7aa09a09a2524e8a776ee564b9f33a0559b)

**恢复时机与范围已确认。** 单次请求或回执丢失时，持久确认位置仍可用就沿原记录继续普通同步。Runner 被判定离线后重新连接时，按 Server 已登记的原 Task 核对当前未结算、有待同步记录或已有明确缺项的工作。增量位置无法接续时，同样对受影响的 Task 做全量对账。已同步完毕的历史 Task 在发现具体缺项或显式发起对账时纳入。某个 Task 对账超时时保留原缺项待补，其他 Task 仍可继续核对。普通任务领取和控制消息在对账期间照常交换。全量方式单独运行时，日常同步也使用相同的 Task 范围。 [一期同步 API](server-runner.md#一期同步-api)

业务操作以本地持久受理、对端持久收到和业务已应用或拒绝来说明进度。接收回执只确认实际落盘的内容，业务结果沿原操作 ID 查询。响应丢失后，同一 ID 可以读回已提交的结果。已对外发出但结果不明的动作沿原操作查证。实际停妥、slot 归还和后续独立工作依据 Runner 本机记录推进。

**已确认。** Runner 首期在托管 workspace 之外保留提交时的交付原件，Server 确认持久接收后仍保留副本。Server 可从原 Runner 通过全量对账补回缺失的文件。交付副本的自动清理留待后续。 [交付副本与全量补回](server-runner.md#交付副本与全量补回)

旧 Tend #389 的回归从真实入口复现过文件归档故障。SQLite 仍能写入，过程片段归档失败却让终止事件无法入账。独立过程片段留下事件序号缺口时，后续终止事件同样受阻。 [旧归档故障回归](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/worker-process-diagnostic-archive-isolation.md#L1-L7)

**已确认。** 例如 Runner 已完成 Task A 并本地保存报告，重连后报告上传中断。Server 可以先提交实际收到的 A 的 Task 结算、停妥证据和完整留言，保存报告引用并显示「文件同步中」。文件完整落盘后按原文件身份标记可下载。需要读取报告的后续工作等该文件可用，同一 Runner 上 Task B 的留言、结算与待领取输入仍可独立交换。 [一期同步 API](server-runner.md#一期同步-api)

**已确认。** 报告在 Runner 本地持久受理后立即进入待同步范围。原 Runner 在下一次与 Server 成功通信的后台同步中登记引用并发起独立上传，Run 仍执行或已经停妥时均适用。传输中断或 Runner 重启后，恢复通信的第一轮同步沿原文件身份继续。Server 已完整保存时，重复请求读回原文件的接收事实。

对已确认的文件，Server 在定向全量对账或读取时发现原件缺失，就把原文件身份列入原 Runner 的待补清单。原 Runner 下一轮同步补回同一份报告。若 Agent 本地从未提交报告，由 Owner 按当前目标决定是否需要补做交付。 [交付副本与全量补回](server-runner.md#交付副本与全量补回)

沿已有离线 fixture 验证 Server 提交收件或业务结果后丢失响应，再只用全量对账读回原操作、补齐缺失文件并保持一份可见交付。延迟首次传输文件时，核对来源 Task 的已收到结算和文件可用状态，以及另一个 Task 的消息投递和 Runner 停妥回报。 [首期验收](server-runner.md#首期验收)

Smoke 用保留原派发的 SQLite 快照模拟 Server 缺少已确认的结算和文件，也单独验证了结算存在时的文件补回。两项都沿 Server 已登记的原 Task 对账。 [全量对账补回已确认交付](plans/full-reconcile-smoke.md#执行结果)

## 工作 Runner 离线时的主 Chat

旧 Tend 由 Server 在本机托管主 Agent。#174 将 Issue Manager 迁到 Runner 领取和执行时仍保留这个入口，因此主 Agent 的模型回合独立于工作 Runner 的容量。旧版受控升级计划在切换 Server 前等待主回合及工具回调完成。 [旧主 Agent 部署](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/deployment.md#L25-L27) · [Issue Manager 的迁移边界](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/runner-issue-managers.md#L5-L11) · [旧升级路径](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/controlled-upgrades.md#L35-L40)

**已确认。** Router 属于 Server，由 Server 在本机使用 Codex App Server 按需执行。工作 Runner 全部离线时，主 Chat 仍可以回复和受理委派。Router 自身暂不可用时，Server 保留原输入，恢复后沿原消息和回执接续。 [Router 托管与恢复](chat-routing.md#router-托管与恢复)

复用主链 fixture。断开工作 Runner，核对 Router 本机回复、委派排队和恢复后的唯一领取。模拟 Router 的 App Server 暂不可用，核对 Chat 收件与历史读取，恢复后沿原消息查到唯一的回复和派发结果。 [主 Chat 进度](chat-routing.md#用户在-chat-中看到什么)

## Server 与 Runner 的独立升级

旧 Tend 的 `--server-only` 允许兼容 Runner 保持原 Run，同时仍协调确认重启窗口。同机 Runner 的重叠交接让新实例领活、旧实例收尾，每代各有 journal。内嵌 tsnet 的场景为后继配置 `tailscaled` 旁路。后来协议 7 将工作执行授权保存在 Runner，Server 短时断开时可继续本机执行。 [旧 Server 单独更新](https://github.com/GhostFlying/tend/blob/6befc2b8/docs/operations/controlled-upgrades.md#L13-L17) · [旧同机交接](https://github.com/GhostFlying/tend/blob/6befc2b8/docs/operations/controlled-upgrades.md#L400-L408) · [协议 7 的离线执行](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/offline-runner-execution.md#L11-L15)

**身份已确认。** 旧 Tend 先按 `host_id` 存单一运行记录，后来为并行收尾实例另加登记。新版 Host 和 Runner 各有 ID，Server 在同一 Host 下分别查到前驱和后继。顺序升级沿原 Runner 接续。并行接替时按顶层 Issue 树确定已有工作的归属。 [旧 Host 运行记录](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/store/migrations/0027_runner_runtime.sql#L3-L10) · [旧收尾实例登记](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/store/migrations/0137_runner_journal_ownership.sql#L7-L17) · [Host 与 Runner 身份](server-runner.md#host-与-runner-身份)

**已确认。** 首期各实例在本机顺序替换，接受短暂断线。外置 Go tsnet、LAN 直连和宿主机 `tailscaled` 对等承载日常通信。前驱和后继各自完成鉴权和同步，可以共用外置 tsnet 通道。 [通信通道与独立升级](server-runner.md#通信通道与独立升级)

**外置进程职责已确认。** 外置 tsnet 负责节点和连接转发，独立发布和重启。新旧 Runner 各自接入并发布端点，业务身份、工作归属和回执留在 Server 与 Runner。 [独立 tsnet 进程](engineering.md#独立-tsnet-进程)

旧 Tend 的协议 6 Server 用真实发布的协议 5 Runner 验证只更新 Server，核对原 Run 的续期、控制和结果。协议 7 继续用发布的协议 6 Runner 验证这一方向。旧版 `--server-only` 还需要在线 Runner 确认重启窗口。 [历史 Runner 验证](https://github.com/GhostFlying/tend/blob/6befc2b8/docs/plans/rolling-runner-generations.md#L5-L17) · [协议 7 验证](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/offline-runner-execution.md#L23-L25) · [旧重启窗口](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/operations/controlled-upgrades.md#L13-L17)

**协议窗口已确认。** 沿用旧 Tend 的 `N` 和 `N−1` 兼容窗口。服务器升代前以目标版本比对未退役 Runner 的持久协议登记。还有使用 `N−2` 或更早协议的 Runner 时，拒绝本次 Server 升级并指出具体实例。已有兼容 Runner 可以继续本机工作，跨过 Server 的短暂重启。首次发布只有 `v1`。 [Server 升级兼容窗口](server-runner.md#server-升级兼容窗口)

复用主链断线 fixture。工作执行期间单独重启 Server，核对原 Runner 推进、Router 从持久输入恢复和结果唯一回传。单独替换 Runner 时，核对原 workspace 归属、等待条件和未同步回执的接续。

## 并行接替的 Issue 归属

旧 Tend 把 workspace 的调度归属固定到 Host，切换新领取权后保留原 Run 和 journal 给收尾实例。退役后，后继可以沿原目录和身份继续只读历史或尾部对账。 [旧 workspace 调度条件](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/scheduler/scheduler.go#L182-L185) · [旧并行接单和原 Run 归属](https://github.com/GhostFlying/tend/blob/6befc2b8/docs/plans/rolling-runner-generations.md#L3-L7) · [旧退役后的对账](https://github.com/GhostFlying/tend/blob/6befc2b8/docs/plans/rolling-runner-generations.md#L41-L43)

**已确认。** 常规派发沿整棵顶层 Issue 树的 Runner 归属执行。同机升级接替限定原 Host 上已确定的前驱和后继，前驱保留已启动的 Run，后继领取已交接 Task 的下一次 Run。 [同机 Run 交接](server-runner.md#同机接替时的-run-交接) · [匹配时机与预分配有效期](server-runner.md#匹配时机与预分配有效期)

| 切换时已知的工作 | 处理 |
| --- | --- |
| 已绑定前驱，Run 正在执行、启动待确认或停妥待确认 | 沿前驱、原 Task 和 Run 继续处理。确认停妥并持久交接后，由后继接续下一次 Run。 |
| 旧树里的 `waitfor` 或已排队待续跑 | 原 Task 的等待条件、待处理输入和 workspace 占用随接续记录交给后继，目录继续由原 Task 独占。交接后条件命中，由后继直接唤醒登记等待的 Agent。 |
| 已接替 Issue 树的新派发、后续回合或重开 | 在原 Host 由后继承接。已领取 Task 的输入按该 Task 的接续归属投递，发给活跃原生回合的 Steer 保留实际目标。 |
| 还未绑定的顶层 Issue，确认未投递或旧分配已拒收且失效 | 进入新一轮分配，合格的后继可领取。 |
| 顶层首次派发可能已送达，领取回执未到 | 沿原分配核对接收结果，证实持久领取后按实际归属续作，证实未接收且旧分配已失效后重新匹配。 |
| Task 已结束且 Run 已停妥 | 前驱保留原历史及尚未同步的原件、回执，供查询和对账。 |

### 前驱 Runner 退役

旧 Tend 同机接替时，接单切换可以先完成。前驱在旧 journal 中的任务和回执收尾、原进程实际退出后才退役。接单实例随后还能从原 journal 读取只读历史和继续部分尾部对账。 [旧退役条件](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/operations/controlled-upgrades.md#L400-L408) · [旧历史接续](https://github.com/GhostFlying/tend/blob/6befc2b8/docs/plans/rolling-runner-generations.md#L37-L43)

**已确认。** 前驱让原 Run 跑完并确认停妥，将排队和 `waitfor` 等需继续的 Task 交给后继，回传收尾后自然退出。Server 确认退出后登记退役，后续升代预检按退役登记确定范围。 [通信通道与独立升级](server-runner.md#通信通道与独立升级)

用户追加独立工作时，Router 可引用原交付建立新顶层 Issue。需要沿旧 Issue 和 workspace 继续的工作，在原 Host 由已接替的 Runner 承接。 [重开后的接续](task-lifecycle.md#重开后的接续)

### Run 边界更换 Runner 的调研

旧 Tend 的调度用 Host 确定 workspace 的位置，新 Run 在领取时登记实际 Runner 的 journal。并行接替后，旧 Run 及其回执继续归前驱，同一 Host 的后继可以接新 Run。外部等待续跑也在领取时连接到后继 Run。 [旧 workspace 调度](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/scheduler/scheduler.go#L180-L185) · [旧 Run 归属及等待接续](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/runner.go#L531-L543) · [旧领取切换用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/rolling_runner_test.go#L227-L239)

**本机实测。** 在禁外网的 Linux 容器中，让 Codex App Server 0.171.0 独立监听 Unix socket，通过 WebSocket 接入多个客户端，使用本地固定 Responses fixture。首个客户端完成 turn 并断开，后继用 `thread/resume` 继续原 thread，核对 `cwd`、原 turn 历史及新的 turn ID。再让前一客户端只取消原 thread 的订阅，保持连接，以另一个客户端续跑。三次 turn 均完成，各客户端按实际回调身份保存一条本地 SQLite 回执，退订后的客户端仍可调用普通 API。App Server 全程在同一个进程中运行。 [Codex 的 thread 接续和工具恢复](https://learn.chatgpt.com/docs/app-server) · [Unix socket 传输实现](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/app-server-transport/src/transport/unix_socket.rs#L175-L211)

**范围已确认。** 这项能力服务于同一 Host 的 Runner 升级接替。Issue、Task、Owner 和 workspace 沿原身份接续，原 Run 的历史、控制和回执沿实际执行归属查证。 [同机 Run 交接](server-runner.md#同机接替时的-run-交接)

产品验证可用同一 Task 等 CI 的场景。前驱完成旧 Run 并保留待回传记录，后继取得接续权，CI 命中后在原 worktree 启动一次新 Run。核对 Task 持续独占目录、旧回执沿原 Run 补传、后继形成自己的新操作及原 Chat 的结果关联。已完成的本机探针覆盖 App Server 接续与回调，Tend 的接续权和实际 workspace 写入由产品验证覆盖。 [一期同步 API](server-runner.md#一期同步-api)
