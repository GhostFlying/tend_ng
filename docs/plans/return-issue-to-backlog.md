# 开放 Issue 原地退回待安排

## 目标和边界

在不替换 Issue 的前提下，让已开始且仍开放的 Issue 经 CLI、用户 API 和 Router Agent 工具退回待安排。退出待安排需要显式 `start`。保留原 UUID、短号、Chat 和责任链、Task 与 Run 历史、交付原件及 PR 关联，不通过取消或重建 Issue 实现。Web 看板继续只读，用户 API 遵循现有会话与同源写入校验。范围止于合入 `main`，不部署，不使用真实业务 Issue 或无关 spike 作为测试数据。

## 已核实的实现边界

- 首次待安排目前由没有 Task 推导，`start` 只处理最初通过 Router 记录的顶层 Issue。退回后已有 Task 必须留档，因此待安排状态不能再仅从 Task 的存在性推导，也不能把 Task 停止当成 Issue 取消。
- 通用 Stop 已区分未投递派发、本地排队、等待与活跃 Run。未投递可由 Server 撤回，其他由原 Runner 查证停妥。复用控制、受理和实际停妥证据，不以 API 回执推定 Run 已停止或 workspace 可复用。
- 即时输入、到期的子项窗口、外部事件和复审不是同一个入口。除派发选取外，需核查后继 Task 创建、待处理输入投递、同机交接和迟到委派。保存原来源、请求及执行事实与阻止调度是两件事。已绑定原 Task 的版本决定或普通 Issue Attention 不可静默改投到其他 Task。
- 看板投影和入列时间在业务事务内更新。保留历史证据，遵守既有运行事实、终态和读写权限。重试沿原操作身份查证，不得把旧回执重放成新的退回或开工。

## 已确认的行为与实现

原 Attention `11ed1557-80fe-435c-b998-457b2558f547` 的持久答复为 `1`。退回在同一事务封住所选子树并提交旧 Task 的正常 Stop。已发出的分配等原 Runner 停妥或未领取证明，未投递分配可直接撤回。Stop 解除等待和目录独占，不回收目录，不删除原事实，不改变 Issue 的业务开闭。看板仍显示事实上的运行，Run 停妥后显示待安排。显式 `start` 需等原子树停妥，只重开目标的调度资格，在原 Issue 创建带前任链接的新 Task，不自动复活旧子项。后续普通收件不为尚在待安排的 Issue 创建后继。绑定审阅保留原候选与审阅轮次，不能用普通 `start` 自动复审。

添加幂等 `POST /api/v1/issues/{issue}/backlog`、`tend issue backlog <Issue> [operation-id]`、原 Chat 作用域内 `tend_backlog_issue`。退回回执带本次涉及的 `stop_control_ids`，其实际进展可由 `tend control read` 查证。`start` 新调用也带操作 ID，旧无请求体入口保留首次登记 Issue 的行为，不用于重启已退回 Issue。保留 Web 看板只读与浏览器写 API 原有同源策略。不会把迟到的旧 Run 业务状态写入当作新接续的决策，原操作记录和能独立保存的事实继续保留。

同一目标的即时输入、明确控制与显式开工在业务事务内释放已受理的先前批次。按已持久接收的顺序提供输入，释放不创建额外 Run。Stop 或退回先封住调度资格，再按该业务结果输出；旧精确等待、审批或要求原 Task 接续的输入仍留在原身份。重新入列与既有看板投影在同一提交更新。

## 补充验证：同一 Issue 的合并窗口与操作顺序

记录、变为有序待送、实际唤醒分别检验。子消息与父级 `issue_inputs` 在 Server 同一事务落盘，讨论读取按已提交的消息顺序且不投递。改动前即时输入的 `wake_at` 为空，不关闭已有窗口，Runner 取件优先选择即时输入，后到的普通事实还可加入旧窗口。GitHub 走独立的 `external_facts`、`external_deliveries` 和批次，批次释放才进入目标输入队列。Runner 本地接收后才把可续跑 Task 排队，实际启动和输入消费仍以 Run 证据为准。

补充要求是 E1、E2 可共用窗口。接受同一 Issue 的非合并操作 C 时先使先前已接受事实有序待送，再应用 C。E3 另起窗口，重传 C 不再次切断窗口。按 Server 收件顺序与可证实的因果关系，不比较不同 Host 的时钟。普通读请求不提前投递或唤醒。Stop/退回不因 flush 取得调度资格。实现时需要纳入独立的外部事实入队边界，不把 Issue 讨论可读等同于原接收 Task 已收到。

对齐加入 Codebase 评论的主干 `e932bea` 后，子输入、匹配的 GitHub 投递和可投递的 Codebase 评论在各自首次入账处取得同一 Server 的持久序号。即时操作将此前被各窗口暂存的输入释放给同一目标，保留评论基线、不可变来源及旧 Runner 能力门槛。已过去的跨队列收件顺序不回填推测。重启或离线回放按原 Server 收件与已证实的因果关系处理。

先添加最小子消息路径 fixture。使用真 Server 与回环 HTTP，在临时库内制造一对父子 Task，Runner 同步输入由固定 payload 驱动、不起模型。通过 API 接受 E1/E2、读取父讨论、提交 C、接受 E3 并重放 C，分别记录可见顺序、可投递顺序和 Run 数；再沿原控制 ID 模拟 Runner 停妥，确认保留事实且不再投递给已停 Task。默认断言目标保序，另提供显式基线模式复现当前反序和合批差异。

在当前主干 `81fdb47c0fa4d4f48c0f7349a54a1f670f439d49` 构建 `cargo build --locked -p tend-server`。`python3 tests/e2e/issue_input_order.py <tend-server> --expect-current-gap` 通过，确认父讨论已可见 E1/E2、C 先被提供给 Runner、E3 加入旧窗。默认验证在目标断言处失败，实际待送为 `[C]`，而目标是 `[E1, E2, C]`。此用例不证明真实 Runner 启动、外部事实保序或待安排后的调度行为。

## 验证计划

- 使用隔离 SQLite fixture 和本机临时 Server/Runner，分别验证已运行、Server 排队、Runner 排队和 `waitfor` 状态；断开 Runner 时核对控制仍待送达，重连后核对同一控制与实际停妥。目录独占按确认后的语义验证。
- 在待安排期间送入迟到的子消息、停妥、交付和外部事实，验证来源仍可读，且不会凭到期窗口、旧输入或重放获得调度资格。显式开工后验证原 Issue 短号、历史、PR、合适的原责任及输入，旁支不受影响。
- 覆盖 CLI 短号与 UUID、API 幂等及认证、Router 原 Chat 作用域、看板阶段与入列顺序。运行 `cargo test --workspace --locked`、`cargo fmt --all --check`、新增跨进程 fixture 及现有控制和等待回归；如未改 Web 源码，使用权限回归验证不新增看板写控件入口。

原始主干 `e2a5f2623abd556cf9a54f6afd8ef074f206f8c0` 已通过 Rust 格式检查、workspace 构建和测试（51 通过、1 个既有忽略），以及隔离 `tests/e2e/control.py` 和 `tests/e2e/external_wait.py --cancel`。这些是现有停止行为的基线，不是退回待安排的验收结果。

本地候选验收使用 `cargo fmt --all --check`、`cargo build --workspace --locked`、`cargo test --workspace --locked`，以及 `tests/e2e/issue_input_order.py`、`control.py --backlog`、`external_wait.py --backlog`。控制、原等待/取消/同机交接、晚到子项、审阅取消后重开、Codebase 评论与 Web 会话/Runner 协议用已有隔离 fixture 回归。没有连接业务数据库或部署环境。

## 独立审阅后的修订

`02556dfd3a7f32b4cb412659f242fcc36b18779c` 的审阅结论为 `changes_required`，保留该轮结论。修订并在同一 Review Task 复审以下四点。

- 已投递结果未知的 assignment 保持原身份。同步在同一证据快照中为每个重发且有 Stop 的 assignment 带上自己的 Stop，即使控制普通分页已满，Runner 原子落盘后不会开旧回合。
- 原 assignment 的 Stop 未结算时，原 Run 即使先自然结束，也不能被自己的已释放输入或迟到事实读回来创建后继。Stop 结算保留原实际 Run 终态与输入。
- 即时释放前缀保留独立的批次身份，不沿用旧的计时窗。支持整批的 Runner 一次获得该前缀及同次即时输入，之后的普通输入另起窗。旧 Runner 继续过滤不支持的事实种类。
- 普通 Stop 将已接受的 GitHub 批次释放到原 Task 的记录，但立即禁止该 Task 接续。等原停止证据后，关联原 Chat、尚未消费的 required 投递沿既有路径回退 Router，且后继不会重复领取。待安排则保留在任务树内，不触发 Router 回退。

新增 `python3 -B tests/e2e/issue_stop_races.py <tend-server> <tend-runner>`，覆盖断线第一响应、Stop 跨页及真实 Runner 不启动和回传、原 Run 自然结束先于 ACK、普通 Stop 与待安排的 required 路由差异，以及混合 32 和 36 条暂存事实加即时输入的批次与既有能力门槛。
