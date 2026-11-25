# Server 与 Runner 的执行和同步契约

Server 和 Runner 是两个独立运行的实例，各自持久化、启动、重启和恢复。两端定期交换任务、控制消息、接收回执、进展和结果。Runner 通过心跳上报联系新鲜度和可用性，供新任务分配参考。本机执行和本机恢复从各自持久记录开始。

## Host 与 Runner 身份

**已确认。** Host 是运行 Runner 的部署主机，使用稳定的 `host_id`。Runner 是独立持有本地数据和执行状态的实例，使用稳定的 `runner_id`，登记时关联所属 `host_id`。一个 Host 可同时登记多个 Runner。Server 可以按 Host 列出各 Runner 的 ID、版本、最近联系和可用容量，派发、控制、工作归属和同步按实际 Runner 记录。两种身份都独立于地址和通道。 [通信通道与独立升级](#通信通道与独立升级)

普通重启或本机顺序升级时，Runner 沿自己的本地库和 `runner_id` 接续。同机并行接替时，前驱和后继以各自的 `runner_id` 登记在同一个 Host 下，分别持有本地执行记录。Run 按实际执行的 Runner 查证，workspace 占用沿原 Task 保存。 [同机接替时的 Run 交接](#同机接替时的-run-交接)

## 通信通道与独立升级

**已确认。** 外置 tsnet、LAN 直连和宿主机 `tailscaled` 是一期对等的通信方式。部署可按实际可达性配置，任一通道都可承载同一套 Tend API 和同步协议。信道按可信处理，请求用预共享 token 鉴权。业务身份独立于网络入口和 tailnet 节点名、tag、WhoIs。重连或切换通道后沿同一 Runner、Task 和操作身份对账。

**外置 tsnet。** Go 通信进程独立持有节点状态，负责入网、监听、拨号和转发。它独立于 Rust 业务进程发布和重启。鉴权与业务协议由 Server 和 Runner 的 API 处理，Server 按 `runner_id` 查询对应的执行端点。 [独立 tsnet 进程](engineering.md#独立-tsnet-进程)

**本机顺序替换。** 一期 Server 与 Runner 独立更新，允许短暂断线。Server 重启期间，已领取的工作由原 Runner 本地推进。Runner 更新时暂停接单，待本机活跃执行可停妥或接续后替换，排队、`waitfor` 和待同步记录从原本地库恢复。顺序替换使用一条可用通道即可恢复同步。

**同机并行接替的部署要求。** 前驱仍运行时，前驱和后继须能分别向同一 Server 鉴权和同步，各自保有可查询的 Runner 端点。两代可共用外置 tsnet 通道。禁止把新协议 Runner 接替到旧协议 Runner。接替期间的原执行和 workspace 占用沿原领取证据确认。

**自然退役。** 前驱在原 Run 停妥、未完成 Task 的接续已交接、待回传记录收齐后自然退出。Server 确认原进程退出后登记退役，移出其升代预检范围。原 Runner 的数据和 workspace 按既有保留规则保存。 [同机接替时的 Run 交接](#同机接替时的-run-交接)

### Server 升级兼容窗口

**已确认。** Server 当前协议代为 `N` 时，接受 `N` 和 `N−1` 两代 Runner 通信协议。首次发布只有 `v1`。当前为 `v2`，仍接受 `v1` 的既有同步。绑定审阅分配和回传仅走 `v2`，组内超过旧 Runner 单页上限时须使用 `v2` 才能完整同批收件。Runner 在普通同步请求中声明使用的协议，Server 按同一窗口校验新登记和重连。

以目标 Server 的协议代确定升代窗口。在停止当前 Server 前，预检读取本机数据库中各未退役 Runner 的协议登记，包括暂时离线及仍在收尾的实例。若仍有 Runner 使用 `N−2` 或更早的协议，拒绝本次 Server 升级，报告 Host、Runner、已登记协议和目标兼容范围。窗口内的 Runner 可以带着已领取的工作跨过 Server 重启。

`tend server upgrade-check <target-protocol>` 返回 `target_protocol`、`compatible_range`（含 `min` 和 `max`，两端均包含）、`allowed` 及 `blockers`。每个阻塞项包含 `host_id`、`runner_id` 和已登记的 `protocol`。例如目标 3 的兼容范围为 2–3。

例如当前 Server 为 `v2`，仍有 Runner 使用 `v1`，则升到 `v3` 的请求会被拒绝。先让该 Runner 更新到 `v2` 并完成同步，再切换 Server 到 `v3`。切换后 `v2` Runner 可以继续领取、收控制和回传原工作的结果。 [旧 Tend 滚动升级](https://github.com/GhostFlying/tend/blob/6befc2b8/docs/plans/rolling-runner-generations.md#L3-L9)

## 派发与本地接收

Server 把 Agent 的派发决定保存为一份可独立执行的任务，包含原任务和 Issue 身份、目标、所需输入、授权范围及预期终点。任务需要 workspace 时一并保存绑定。Runner 持久接收后即可执行，接收回执随后补传。相同任务重复送达时，Runner 沿原身份返回接收事实。双方重启后从各自的原任务和接收记录恢复。

Runner 可以与 Server 断开，自行推进已领任务所需的多轮执行，保存原进展、操作回执、结果和终态证据。工具写入落盘即返回操作 ID。Agent 以带事件条件的 `waitfor` 请求暂停当前执行。条件命中或收到需要唤醒的输入时，沿原 Task 调度。 [Agent 工具与本地受理](agent-tools.md) · [等待期间的用户输入](task-lifecycle.md#等待期间的用户输入)

Server 把采集后的待投递 PR / CI 事实保存在实际收件队列。发给 Router 的事实由 Server 本地投递。Runner 通过定期同步持久接收发给自己所托管 Agent 的事实，沿原目标投递。回执沿原事实及收件关系对账。 [PR 和 CI 事实的采集与投递](design.md#pr-和-ci-事实的采集与投递) · [任务等待与续跑](task-lifecycle.md)

Server 可以在 Runner 离线期间接收 Chat、保存新委托和停止请求。主动停止在 Runner 收到控制消息后本地执行，实际停妥根据执行证据确认。接收结果未知时，沿原派发确认领取结果。最后联系、最近工作进展、回传缺口和停妥情况有各自的事实来源。

## 执行 slot

Runner 在启动前为本次实际执行持久记录 Run 身份。同一个 Run 可以包含多个原生回合。恢复同一次执行沿原 Run 对账。确认上次执行停妥并再次启动原 Task 时，使用新的 Run 身份。控制回执和停妥证据沿对应 Run 记录。

slot 的运行统计只包含 Runner 已确认正在执行的 Agent。预分配、排队、等待和待同步各自记录。启动或停止尚待确认时，沿原执行保留待确认状态。Runner 按本机准入和停妥证据上报实际可接单容量。确认执行停妥即可归还对应 slot，业务回执随后同步。Run 正常结束时，Runner 按本地结算和续作记录判定原 Task 是否一并完成。 [Run 正常结束时的 Task 结算](task-lifecycle.md#run-正常结束时的-task-结算)

## 分配与投递

**Server 决定分配，Runner 定期拉取给自己的派发。** Router 发起顶层 Issue 的派发，Server 按已明确的 repo 和工作能力筛选 Runner，并匹配 `issue-router`。Issue Agent 通过工作子 Issue 委派，Server 在该顶层 Issue 所属的 Runner 上匹配 Work Agent。每份派发写明目标、归属和优先级。Server 核对任务要求、既有 workspace 归属和 Runner 的可用容量，再持久记录分配。Runner 可在心跳或同步请求中取得派发，核对本地可执行条件后受理并落盘。

建立或交接 Issue 责任时，Runner 持久领取该分配后，所选具名 Agent 的 Owner 责任生效。Server 根据原领取回执更新对外状态。顶层 Issue 和工作子 Issue 分别记录 Owner 和领取回执，续作沿各自的当前 Owner。子 Issue 已领取的任务在本地独立推进，父 Issue 通过同步读取结果。 [Agent 角色](design.md#agent-角色)

任务已分配和 Runner 已持久接收是两个事实。心跳变旧影响后续分配，已接收的任务按本地契约继续。原 Runner 可能已接收时，沿原派发对账。原派发已失效且有证据确认未持久接收时，尚未绑定 Runner 的顶层 Issue 可以重新选择 Runner。

Runner 取件按自己的待投递记录查询。心跳可与同步一起上报，按需要保存最新可用性观察。

旧 Tend 的 `runner.claim` 由 Runner 发起，Server 在领取时挑选工作。部分空领取曾进入写事务查找候选与恢复路径，后来添加了只读预查。新设计需保持取件和原任务查询有界，候选计算与实际分配围绕当前任务及 Runner 的事实进行。 [旧领取预查](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/claim_preflight.go#L39-L75)

具名 Agent 的选择和 Runner 的候选匹配见 [具名 Agent 与任务分配](agent-selection.md)。

## Issue 树的 Runner 归属

**已确认。** 顶层 Issue 首次由 Runner 持久领取时，确定整棵 Issue 树的 Runner 和 Host。常规派发、审阅、续跑和重开沿该归属调度。缺 slot 时在所属 Runner 排队，Runner 暂不可用时等它恢复。Repo 或 Agent 配置缺失时，保留实际原因并送回负责判断的派发方。不同顶层 Issue 各自选择 Runner，主 Chat 的 Router 沿自己的配置运行。 [无候选时的反馈](agent-selection.md#无候选时的反馈与改选)

Server 应用顶层 Issue 的原领取回执时，同一次持久提交 Owner 和 Runner 归属。Runner 可以先在本地受理子项委派。Server 若先收到子项操作，保留原操作，取得顶层领取事实后再应用子项分配。全量对账和增量同步沿原领取和接替记录恢复归属。

远期如果工作目标本身需要多台设备上的 Agent 协作，再单独设计跨设备子 Issue 的显式派发和结果回传。

### 同机接替时的 Run 交接

**已确认。** Run 边界的交接用于同一个 `host_id` 上已确定的新旧 Runner 接替。切换接单后，后继接新顶层 Issue 和已接替 Issue 树的后续派发。旧派发的领取结果待确认时先沿原分配查证。 [并行接替的 Issue 归属](design-questions.md#并行接替的-issue-归属)

已启动的 Run 由前驱执行到停妥。已领取的 Task 需要继续时，前驱在确认本次执行停妥或尚未启动后，持久交出后续执行权，经普通同步提交给 Server。接续记录携带原 Owner、目录绑定、待处理输入、等待条件及开工所需的回执。后继持久领取后接续原 Task 的待执行或等待状态，实际启动时使用新 Run 和已有的 workspace。处于 `waitfor` 的 Task 可以在条件命中前完成交接，目录持续由原 Task 独占。 [命中事件与续跑](task-lifecycle.md#命中事件与续跑)

交接期间到达的用户输入或等待命中沿原 Task 持久保存。接续生效后，后继按原身份消费仍待处理的输入。指定旧 Run 的控制、历史和回执沿原执行查证，原文件和回执仍由前驱独立回传。本次实现仅在原 Runner 的原分配中复用 thread。换 Runner 后沿原 Issue、Task、待处理输入和交接回执开新 session，不把原生会话转移作为接续前提。 [原生接续调研](design-questions.md#run-边界更换-runner-的调研)

## 匹配时机与预分配有效期

**已确认。** Server 应用派发后，立即用所需 category、实际工作要求和最近还有效的心跳尝试分配。已有合格候选就记录预分配，缺少候选就记录等待原因。Runner 的下一次心跳或同步可以更新候选事实，并拉取已分配任务。派发工具先返回本地受理，Server 的匹配结果沿原操作异步返回。

| Server 已知事实 | 超时或失联后的处理 |
| --- | --- |
| **已预分配，确认还未交付** | 预分配有取件截止时间。到期或 Runner 可用性过期时，Server 撤回原预分配，用原任务和原要求重新匹配。已绑定的任务沿 Issue 树当前归属匹配。 |
| **任务可能已交付，接收回执未到** | 原分配保持接收待确认。通过后续同步核实原接受或拒收的事实。长时间未确认时向派发方提供一次原任务反馈，心跳超时更新联系状态。 |
| **Runner 已持久接收** | Runner 按本地任务契约继续。结果、停妥及回执由实际事实确认。 |

Server 首次返回任务前，在同一个短事务中确认分配仍可投递、期限有效，并持久记录这次分配可能已交付。到期撤回与首次投递对同一分配做互斥的状态变更。撤回先提交时，迟到的请求读取当前可投递记录。投递先提交时，用原派发身份等待接收对账。若根据未接收证据重分配，还要确认旧派发已失效并会被拒收。

取件截止时间由 Server 按普通通信周期配置和执行，重启从持久记录恢复到期处理。心跳更新可用性，待取件期限按原分配计算。其他任务照常匹配。执行已开始时的停妥与交接沿原任务证据确认。 [旧离线执行中的心跳与停妥边界](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/offline-runner-execution.md#L11-L19)

## Repo 注册

**Repo 在 Runner 注册，Server 读取各 Runner 上报的可分配清单。** Runner 持久保存本机仓库登记，使用自己的 Git 访问配置准备 workspace。注册可以关联已有本地仓库，也可以登记由该 Runner 准备的源。具体目录、访问配置和凭据由本机管理。

需要仓库的任务引用所需 repo 的稳定标识。同一 Runner 须能访问派发所需的全部仓库。Runner 通过普通同步报告 repo 标识和可用状态，可以随心跳上报变化。Server 从各 Runner 的登记构建可分配清单。已绑定 workspace 的续作沿现有归属派发。几个 Runner 可以登记同一个 repo，各自验证本机可用性。收到任务时，Runner 核对所需 repo、原 workspace 和本机条件，并记录实际接收或拒收结果。

例如 Issue 树已绑定 A，且任务需要 repo X，Server 核对 A 的登记和可用状态，再将任务分给 A。A 暂不可用时，新任务等 A，已由 A 接收的任务按本地契约恢复或继续。Server 使用自身的数据来源配置采集 PR / CI，并使用同一 repo 标识关联 Issue。

旧 Tend 将 Server 的逻辑仓库登记与 Runner 的本机绑定分别管理，Server 根据已确认的绑定显示可执行主机。新设计由 Runner 的登记直接生成 Server 的派发视图。 [旧仓库登记与本机绑定](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/deployment.md#L35-L54)

## Workspace 分配

**已确认。** 顶层 Issue 需要代码时，在所属 Runner 上共用一个 workspace。子 Issue 默认使用共用的工作目录。上层 Issue Agent 在拆分时按需为子 Issue 指定独立 worktree，Runner 按派发准备目录并记录实际绑定。续跑或更换 session 时沿原绑定恢复。

角色 Skill 指引 Work Agent 使用已分配的目录。工作中发现需要独立 worktree 时，Work Agent 向上级提出，由上层 Issue Agent 决定并在派发中指定。Git 命令保持可用，Agent 按仓库约定使用。 [Agent 工具与本地受理](agent-tools.md#调用接口)

Runner 持久记录各托管工作目录当前由哪个 Task 独占。`waitfor` 等待及待续跑期间，原 Task 继续独占该目录。等待绑定审阅的原作者同样保有写入独占。其他要用同一目录的 Task 保持排队，分配到独立工作区的 Review Agent 可以使用已归还的 slot。Git 审阅目录使用完整提交对应的 detached worktree，且核对实际 HEAD，文件候选从原 Runner 的不可变交付副本复制到审阅目录。若同机接替后本地原件不可用，沿原审阅 Task 报告准备失败，不借用作者 workspace。Task 结束后，Runner 根据实际退出或隔离证据释放占用。 [共享 worktree 的占用](design-questions.md#共享-worktree-的占用)

同机并行接替期间，两代 Runner 按同一份 workspace 占用事实准入。前驱仍在使用的目录继续由原 Task 独占。该 Task 交接后，后继以原占用接续，其他需要同一目录的 Task 继续排队。 [同机 Run 交接](#同机接替时的-run-交接)

### 多仓目录布局

**已确认。** 使用旧 Tend 的 Git workspace 布局。在同一 Runner 上，一个 workspace 的挂载清单可以包含一个或多个 repo。每个 repo 有自己的 worktree 子目录。Runner 按挂载项保存 repo 标识、相对路径、实际工作分支和起始提交。Work Agent 从 workspace 根启动。Runner 在根生成 `AGENTS.md`，列出各 worktree 及其起点，引导 Agent 读取并遵循对应仓库自己的说明。单仓使用只有一个挂载项的同一布局。 [旧 Git 布局](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/gitws/gitws.go#L127-L138) · [角色 Skill 与仓库约定](agent-tools.md#角色-skill-与仓库约定)

### 新 worktree 的起点

旧 Tend 的 Git 后端在创建 worktree 前刷新远端，按显式分支或提交、已配置候选、远端默认分支等顺序解析起点，并报告实际提交。旧用例已覆盖有本地 clone、但 Git 远端暂时不可达时返回准备失败。 [旧分支选择](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/gitws/base.go#L27-L65) · [旧刷新失败用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/gitws/gitws_test.go#L403-L418)

**已确认。** Runner 为每个 repo 登记一条默认主干。用户登记时未指定主干，Runner 从 fetch 得到的远端分支里依次匹配 `main`、`master`、`develop`，保存第一个匹配项作为该 repo 的默认主干。

Runner 在新建 worktree 时按仓库先 fetch，再解析各自的起点。用户启动时可以为所需仓库指定分支或提交。独立的子 Issue worktree 由 Issue Agent 在派发时协调起点，可以引用前一项交付的分支或提交。未指定起点时，从该 repo 本次 fetch 后默认主干的最新提交创建。Runner 保存实际起始 SHA 和 workspace 绑定。已有绑定的续跑沿原目录及其中的工作继续。刷新或解析起点失败时，Runner 沿原 Task 报告具体 repo 和原因，由上级 Agent 决定是否重试或调整起点。 [工作委派与 Issue 结案](task-lifecycle.md#工作委派与-issue-结案)

用首个产品切片的临时 Git 仓库验证默认主干的匹配顺序和显式选择。推进远端主干后新建 worktree，核对实际起始 SHA。已有 workspace 的续跑读取原目录及其中的进度。 [首个产品切片](plans/first-product-slice.md#输入到交付)

### 开工前的 workspace 准备

旧 Tend 在 worktree 布置完成后运行已配置的仓库及 workspace profile，按输入缓存成功结果。脚本失败会使当次准备失败，Work Agent 尚未启动。旧回归用例核对了 profile 失败后的任务状态，也验证了环境修好后可以复用当时已经建好的 worktree。 [旧 profile 执行](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/gitws/profile.go#L21-L59) · [旧准备失败验收](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/e2e/runner_test.go#L943-L953) · [旧 worktree 复用用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/gitws/gitws_test.go#L421-L436)

PR #185 还处理过一条启动前检查。旧版会提交 Git 改动的任务复用有未提交修改的 worktree 时，需要显式声明是否纳入已有改动。缺少声明时，准备流程在 Worker 启动前结束。修复增加了把原诊断交回当前 Issue Owner 的路径。 [旧负责人修正流程](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/workspace-preparation-recovery.md#L3-L13)

**已确认。** Runner 准备可进入的 workspace，完成所需的 Git worktree、目录绑定和外层 `AGENTS.md`，并按所选配置启动 Agent。项目依赖安装、构建和测试由 Work Agent 结合当前任务按仓库约定执行。仓库可以提供常用初始化命令供 Agent 使用。接手已绑定目录时，Agent 从当前文件和原 Task 的授权范围判断如何继续及提交。若 Git 或目录准备失败，Runner 沿原 Task 保存具体原因并回给上级 Agent，由它决定恢复动作。 [执行失败后的接手](task-lifecycle.md#执行失败后的接手)

## Workspace 保留与回收

**已确认。** 顶层 Issue 结案后保留托管 workspace。一期只通过 CLI 显式发起回收，Server 将请求持久投递给所属 Runner。

Runner 核实目标是自己管理的目录，相关 Task 已结束、目录占用已释放，且相关执行已停妥后才回收。回收和同一目录的新占用互斥，结果沿原操作 ID 保存与回传。尚有占用时，记录具体原因供 CLI 查询。

结构化方案批准的候选版本和决定仅由 Server 解释。同步响应可选地向 Runner 公告创作支持。跨 Runner 交接只传原等待义务，批准仍沿既有 `AttentionAnswered` 载荷传给原 Task，不要求旧 Runner 识别新输入种类。

## 一期同步 API

Server 与 Runner 一期通过普通请求完成全量对账与批量增量同步。全量对账可以独立完成同步。增量用于减少重复传输和日常同步延迟，两条路径共用任务、事件、操作和回执身份。

| 路径 | 适用场景 | 契约 |
| --- | --- | --- |
| **全量对账** | 日常通信、离线重连、重启恢复及故障对账 | 按 Task 及关联 Run 枚举领取、控制、进展、终态和停妥证据、业务操作及回执、完整留言和交付文件。按原身份比对并补齐缺项，在增量游标或传输队列缺失时仍可执行。 |
| **API 增量同步** | 正常通信，双方持有已确认进度 | 从接收方已持久确认的位置批量传输新增的完整记录。重试沿原身份去重，确认位置按实际收齐范围推进。 |

Chat 同步以已持久保存的完整对话消息为单位。单次请求可携带多条完整消息以及其他持久事实，任务进行中即可同步。App Server 的 `item/completed` 可以携带完整的原生 `AgentMessage`，`item/agentMessage/delta` 则提供生成中的文本片段。Issue Chat 正文沿显式留言的记录同步，原生过程按需读取。 [Codex 通知名称](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/app-server-protocol/src/protocol/common.rs#L1991-L1996) · [完成与增量通知结构](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/app-server-protocol/src/protocol/v2/item.rs#L1418-L1448) · [Issue Chat 的留言与执行过程](chat-routing.md#issue-chat-的留言与执行过程)

**已确认。** Server 可以先持久接收 Agent 的完整留言、Task 结算、停妥证据及交付文件引用。原件独立上传，文件引用按实际状态显示为同步中或可下载。需要读取文件的后续工作等该文件可用，同一 Task 的其他独立操作和同一 Runner 的其他 Task 继续同步。 [数据归属](storage.md#建议保存范围)

建议每次全量对账先确定一个稳定的记录截止位置。Runner 可继续工作，截止位置以内的记录按原身份比对，缺失内容分批补传。新事实由后续对账纳入，启用增量时也可以从已确认位置接续。Server 对已核实的覆盖范围给出持久回执。遇到原件缺失或同一身份下的事实冲突时，保留证据并报告实际缺口。

**已确认。** 单次请求或回执丢失且持久确认位置仍可用时，沿原记录继续普通同步。Runner 从离线恢复时，按 Server 已登记的原 Task 核对尚未结算、有待同步记录或存在已知缺项的工作。增量位置无法接续时，对受影响的 Task 执行同样的全量对账。已经收齐的历史 Task 在发现具体缺项或显式请求时纳入。某个 Task 对账超时，保留该 Task 的缺项，其余 Task、领取和控制消息继续推进。

Issue 及其关联记录是工作事实的 SoT。来自 Run 的记录按原 Run 和实际执行的 Runner 核对来源。后继领取后，前驱的晚到事实仍沿原执行收件和查阅。Server 根据操作的原授权范围和当前业务条件应用操作。例如前驱迟到的停止回执结算旧 Run，后继的 Run 继续按自己的执行事实更新。持久收齐执行事实和已应用业务操作各有可查回执。批次接收和结算以短事务推进，大原件传输和展示独立进行。

## 交付副本与全量补回

提交交付文件时，Runner 在托管 workspace 之外固定被提交的完整原件，持久保存原文件身份和待同步记录，再返回本地受理回执。Server 完整持久保存文件后更新同一文件的可用状态并确认接收。首次可读的交付按原身份进入父 Task 的五分钟普通窗口。窗内截止固定，重启、重试和补传沿原输入身份消费。Runner 此时可以释放传输临时数据，首期继续保留交付副本与原文件引用。交付副本的自动清理留待后续。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒)

**已确认。** 原 Runner 下一次与 Server 成功通信的后台同步登记待传文件的引用，并发起独立上传。Run 仍在执行或已经停妥时均适用。连接中断后的第一轮成功同步沿原文件身份继续，已完整保存的文件按原记录返回接收回执。

全量对账按 Server 已登记的 Task 核对交付记录和文件的实际可用性。对账或读取时发现原件缺失，Server 将文件列入原 Runner 的待补清单。Runner 在下一次后台同步中取得缺项，从保留的副本补回同一文件。 [数据归属](storage.md#建议保存范围)

全量对账包含 Runner 仍保留的已确认记录。原回执供历史查证，本次补传范围以 Server 当前持有的原记录和文件为准。 [已确认交付的恢复 smoke](plans/full-reconcile-smoke.md#执行结果)

## 按需读取 rollout

Codex rollout 等原生执行原件由实际执行实例保管。Router 原件在 Server 本机保存，工作 Run 原件保存在原 Runner。Server 记录实际执行归属和原件引用。Web 或 CLI 打开、刷新执行详情时，通过共用 API 发起当次读取。Router 详情由 Server 读取本机原件，工作 Run 详情按原 Run 向原 Runner 查询。详情显示实际读取状态，例如原 Runner 暂不可达或原件缺失。

Issue Chat 消息页用已有的 Run 引用呈现折叠入口。用户点开或刷新某段执行过程时，才发起对应 Run 的读取请求。 [Issue Chat 的留言与执行过程](chat-routing.md#issue-chat-的留言与执行过程)

在工作 Runner 配置中可设置 `"inspect": {"listen": "127.0.0.1:8833", "url": "http://127.0.0.1:8833"}`。同机部署可以用回环地址。跨机部署将 `url` 设为 Server 能通过已有可信通道访问的地址，`listen` 由实际转发或绑定方式决定。Runner 心跳公布该地址，Server 按原 Run 使用共用的预共享 token 读取原生回合。未设置地址的 Runner 仍可执行和同步。

## 后续实时流

一期通过普通 API、页面定期查询和 Runner 定期通信完成 Chat、Attention、看板、通知及 Queue 和 Steer。Steer 的实际送达以运行该 Agent 的实例对目标回合的核验和回执为准。

后续流式 Agent 体验从 App Server 的原生增量事件开始。Router 由 Server 直接转发，工作执行经 Runner 和 Server 转发。SSE 用于订阅指定执行的实时事件，具体转发端点在实现时确定。全屏干预的操作继续走普通 API 和持久投递。完整消息和业务回执按原身份持久保存，重连后可通过普通 API 重读。

## 首期验收

- 领取任务后断开与 Server 的连接，验证 Runner 完成原定工作并保存结果与停妥证据。恢复连接前用本机工具可查原操作。同一故障 fixture 覆盖离线重启与 session 替换。
- 在同一 fixture 中分别只重启 Server 和只替换 Runner。核对已领取的工作、Router 原输入、workspace 归属、等待条件和未同步回执按各自的持久记录接续。
- 将分配 fixture 的两个 Runner 登记到同一 Host，核对 Server 同时列出两个身份并分别更新心跳。一个 Runner 断连后沿原 ID 重连，核对两个实例的任务和回执仍能按原归属查询。
- 同机接替时，让前驱用原 Task 等 CI，在交接期间送达匹配事实，再让后继领取接续。核对后继在原 worktree 只启动一次新 Run，等待期间目录仍由原 Task 独占，前驱按原 Run 补传并自然退役。 [同机接替时的 Run 交接](#同机接替时的-run-交接)
- 仅用全量对账和普通 API 跑完主链。保留 Server 的原任务和派发，模拟部分同步状态缺失。Runner 用持久记录补齐领取、执行进展、完整留言、交付文件、结果和回执。对账期间产生的新事实由下一轮同步补齐。重复发送和丢失回执都沿原身份恢复，用户只看到一次派发和结果。
- 在同一 fixture 中中断交付文件的首次上传。核对来源 Task 的结算和停妥证据可先同步，文件显示同步中，同一 Runner 的另一 Task 可收件和回传。恢复连接后，核对完整文件沿原身份变为可下载。
- 在文件已确认同步并回收 workspace 后，移除 Server 上的该文件原件，保留原交付记录。用全量对账从 Runner 补回，核对原文件身份、完整内容和唯一的可见交付。
- 复用该 fixture，打开详情时核对 rollout 查询读取原 Runner 当时的记录。再断开 Runner，核对 rollout 的读取状态以及 Server 上已同步留言和交付文件的可读性。
- 复用同一 fixture，让 App Server 先发出几段文本，然后完成一条消息，任务继续运行。在生成前后查询，核对 API 只呈现已完整的消息，并在任务进行中同步持久进展。重传同批记录后，每条完整消息只出现一次，最终事实与全量对账一致。
- 用边界测试覆盖乱序、缺口及原归属失效后的晚到事实。Server 保留原执行证据，当前 Issue 状态依据实际适用的操作推进。
- 预分配使用可注入时钟的定向用例。顶层 Issue 未绑定 Runner，A 尚未取件便超过截止时间，此时可改派给合格的 B。A 的取件和撤回并发时，只能有一条转换成功。模拟顶层领取回执丢失、子项操作先到，核对恢复后的子项分配沿原领取回执确定的 Runner。
- 复用主链 fixture，核对同一顶层 Issue 的子项默认使用共用工作目录。上层 Issue Agent 显式指定独立 worktree 时，子项在原 Runner 上使用该目录，续跑保留原绑定。
- 用两个临时 Git 仓库定向验证 workspace 准备，一个从登记主干开工，一个指定起始提交。核对各挂载的路径和实际起始 SHA、Work Agent 的启动目录，以及外层 `AGENTS.md` 能指向各仓库自己的约定。 [多仓目录布局](#多仓目录布局)
- 复用父 Issue 先结案的 fixture，核对结案后 workspace 仍保留，实际目录尚有子 Task 占用时回收请求得到明确结果。待关联 Task 结束且停妥后，用显式请求回收已释放的托管目录。

旧 Tend 的离线执行设计提供了本地执行恢复、持久原件和真实停妥证据的参考。 [旧离线执行设计](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/offline-runner-execution.md#L5-L23)
