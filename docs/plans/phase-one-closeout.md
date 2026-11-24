# 一期收尾与验收

一期沿 [总体设计](../design.md)、[执行与等待](../task-lifecycle.md) 和 [Server 与 Runner 契约](../server-runner.md) 的已定边界推进。每段改动直接进入主干，分别记录固定 fixture 与实际服务读回的验证范围。

## 外部事实、等待和批次

- Server 将 GitHub 的 PR 状态、当前 head 的 CI 汇总及有明确身份的检查保存为带版本的事实。投递保存原订阅、等待和事实身份，固定五分钟窗口以首条待投递事实为起点。重复观测只更新观测时间。
- Runner 本地持久接受带具体条件的等待。回合实际停妥后释放 slot，原 Task 保留目录占用，其他目录照常推进。已有匹配事实和权限失败都沿原等待送回同一责任 Agent。普通用户输入继续及时唤醒，Owner 可撤销尚无结果的等待。
- 固定 fixture 核对同一 Runner 上实际停止、原 Task 续跑、新 Run、保留目录、过期检查拒绝匹配和单次投递。测试推进已持久化的批次截止时间，部署保持五分钟默认窗口。

## 后续验收块

- 补齐 Queue、Steer、取消、重开与实际停妥回执。
- 独立运行 Server、Runner 和 tsnet，核对同机交接、原目录和原 Run 归属，以及 Server 的协议预检。
- 在真实本机 Codex App Server 和可用的可信通道核对完整主链、Attention、并行任务和回传。记录部署验证与固定 fixture 的各自范围。

## 回合控制

- Server 持久登记 Stop 与 Steer。Stop 在原 Runner 证实停止后结算；Runner 离线时保留待送控制。Steer 关联精确 Run 和原生回合，并向 App Server 提供本次操作 ID。未送达和回执未知分别保留，用户可显式把未送达的原文改为 Queue。
- Runner 在执行循环检查本地控制表。普通同步和 Task 全量对账共用相同操作与回执。运行中 Stop 使用原生中断并验证进程停妥；排队或等外部事实的 Task 直接结束本地安排。
- Issue 业务取消与重开使用 Issue 状态及明确原因。Owner 能为指定子 Task 提交停止，旧控制沿旧 Task 结算。用固定回合验证活跃 Steer、旧 Run 未送达、停止和回执重放。

CLI 可用 `tend task stop <task-id>`、`tend run steer <run-id> <text>`、`tend control read <operation-id>` 与 `tend control queue <operation-id>`。直接发出的 `tend issue cancel` 为当前树内已知 Task 登记停止控制；Owner 也可按需提交具体 Task 停止，并独立标记 Issue 业务状态。

## 同机接替与升级

- 前驱和后继登记相同的 Host 及同一个绝对 workspace 根。后继可配置为待命。Server 确认替换关系后，前驱禁止从本地领取新的 Run，后继可以接新 Issue。
- 前驱继续实际已启动的 Run。某棵 Issue 树上所有活跃 Run 停妥，并且该树的原领取和回传获得确认后，前驱将该树中可接续的 Task 冻结并同步交接。Old Run 仍由原 Runner 保管。后继按原 Task、等待、委派和目录接续。
- Server 按永久 Runner 记录保存其协议代数。升级前 CLI 提供只读预检。前驱回传收尾后报告已排空，并从 Runner 进程退出。现场验收实际退出，同时确认后继仍能同步和执行。

在两个 Runner 配置相同的 `workspace_root` 和 `host_id`。后继配置 `standby: true`，并使用独立的数据库和 `runner_id`。先用 `tend runner list` 核对两个实例，再运行 `tend runner replace <predecessor-id> <successor-id>`。确认前驱进程已退出后执行 `tend runner retire <predecessor-id>`。升级 Server 前用 `tend server upgrade-check <target-protocol>` 检查仍需兼容的 Runner。

## 原生审阅记录

Runner 从父回合的 Codex 原生协作条目记录子线程和其实际状态。子线程创建、进行中和终态分别保留，父回合可以显式登记所审候选、范围和结论。CLI 从对应工作 Issue 读取观察和审阅结论。固定夹具核对创建、等待期间运行中、终态和重复回放。

## Workspace 的显式起点

Router 或 Issue Agent 在派发里按已登记 repo 提供起点。Runner 先 fetch，再优先解析远端同名分支，也支持明确提交和完整引用。每个新建 mount 保存其实际 SHA。已有 worktree 保留原起点和目录，后续 Task 保留派发的起点记录。固定 fixture 使用主干前进后留下的远端分支，核对实际 HEAD。

## 显式回收 Workspace

CLI 为已结束的顶层 Issue 登记回收。Server 暂停该树的新准入，原 Runner 或已接替该树的 Runner 确认本地无执行或等待占用后，从 Git 的工作树登记中移除托管目录。回收保留分支、执行历史和已提交的交付。存在工作区改动时显示具体失败原因，用户可以保留改动或处理后重新提交。单次请求与结果沿原操作 ID 查询。

## 外部事实的默认收件人

- 保留普通 PR 观测、明确订阅和直接等待。通过 CLI 的原 Chat 消息登记必送条件；Router 可在本地登记主动订阅。两条入口复用 PR / CI 条件和 Server 的只读观测。
- 匹配事实时，从同一 Chat 中已关联的 Issue 树查找唯一的顶层归属。已确定的事实沿顶层 Owner 的队列投递，保留具体关联 Issue。归属未定的事实按 Chat 合成一个后台 Router 输入，优先级低于已排队的用户消息。多个订阅或等待分别保留来源。
- 固定 fixture 核对重复登记、无隐式唤醒、两个收件路径、一次固定窗口内合并，以及 Router 回复沿原消息关联。

## 具名 Agent 的候选与指名

- 默认继续按仓库、category、capability 和可用度分配。查询入口复用分配的资格规则，返回符合的具名配置及其 Runner。Router 使用 Server 的登记和当前可用度，Issue Agent 读取当前 Runner 本地的配置与仓库。
- 顶层派发可显式指定 Agent 及其 Runner。子 Issue 只能在树已归属的 Runner 指名。记录原选择，实际领取仍核对当前配置与版本。缺少合格配置仍保留派发及原因。
- 固定 fixture 以默认排在前面的配置及显式指名的后续配置验证真实分配。Router 模型和 reasoning effort 由 Server 进程选择。

## 固定候选提交与收尾验收

- GitHub 的 PR 状态照常跟踪当前 head。仅对活跃条件显式指定的旧候选提交读对应的 CI，按实际 SHA 和检查身份匹配。没有固定 SHA 的条件跟随当前 head，旧候选的事实留作证据。
- 收尾记录真实 Codex 的主 Chat 交付和 Attention、本地定向跨进程验收、以及两条同步路径下的 API 响应观察。运行入口统一写进 README，配置保存在部署者的本机。

## 角色指引与按需查证

- 把角色指引作为短文本随原生回合载入。复杂工作提醒 Work Agent 寻求独立审阅，保留对审阅方式和结果的自主判断。Review Agent 说明候选、范围、证据和结论。
- Router 在原 Chat 按需读取相关 Issue 的持久事实。只读查证共用用户 API 的读逻辑，继续由 Router 组织给用户的答复。
