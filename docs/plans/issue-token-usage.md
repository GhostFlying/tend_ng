# Issue 树 Token 用量（关联 #8）

## 目标与边界

在 Issue 详情页切换「直属用量」和「含所有后代」。子树中的每一行只展示该 Issue 的直属值，包含当前 Issue 本身。覆盖按 Server 已接受的原生 Started Run，而非 Task、Router attempt 或请求数；有至少一条有效原生请求即算已观测，不承诺已观测 Run 的尾部完整。没有开始与完全未观测分别呈现。保留字段级有效请求数、明确区分未知、显式零及只覆盖部分请求的已知小计。

只采集 Runner 驱动的原生 `thread/tokenUsage/updated.last`。Router、讨论、独立子 Agent 线程、日期统计、估算、历史 Token 回填、定价、预算、告警、导出、看板及逐请求界面不在本次范围。原型中的全局入口、筛选和列表下钻均不迁移。

## 实现方案

- App Server 适配层复用原生回合的 thread/turn 核验，包含回合确认之前和同步 RPC（Steer 与 Stop）期间暂存的通知。控制退出前只派发已经进入 RPC 缓冲且身份相符的用量，不等待后续尾部，不改变停止或其他事件的语义。Runner 从 `thread/start` 或 `thread/resume` 返回的本机 rollout 路径，仅增量读本次原生回合开始后的追加段，比对 `token_usage_record` 的原 thread、turn、原生 `response_id`、已上报计数和累计游标。通知仍是汇总数值的来源，原件只用于证明单次请求和稳定身份。没有证明的快照不会进入待同步队列，当前 Run 的原生记录不用于历史补采。Runner 及时写入本地 SQLite outbox，不依赖同步连通。Codex 追加是异步的，参见[原生 rollout 写入](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/rollout/src/recorder.rs#L1057-L1108)。仅在仍有未匹配通知时借用既有活跃控制轮询（250 ms）检查新追加，使没有后续通知的活跃 Run 也能入账，且不重扫未增长的 rollout。
- 原生通知同时服务上下文占用，压缩重估、窗口已满、速率更新和旧快照重发不是新的请求。仅采集被本轮原生逐请求记录证实的 `last`；累计值只匹配证据，不计算差量。相同 thread、turn、response ID 与字段值合并，同身份出现两个已核验且不同的字段值时隔离冲突并撤销已有贡献。
- 兼容协议 2 的可选用量批次和独立确认位置。任务对账只送该 Task 的事实，无权确认整台 Runner 退场。旧 Server 忽略新字段，新 Runner 保留用量且不阻塞旧协议的其余回执与退场。支持统计的 Server 要求单独的 Runner 全局同步证明退场。
- Server 校验原 Runner、assignment、Run、Task 及 thread/turn；Started 之前的可归属候选先暂存，不影响统计。可信度来自原 Started，不来自活跃 Task 的最新 Run；迟到的原值可以回修已经停妥的 Run。迁移仅从已接受的历史 Started 证据初始化分母并标「采集启用前」，不读取原生历史、不补 Token。
- SQLite 持久保存原始键、冲突/等待状态、Run 小计、Issue 直属与子树小计，以及按祖先和后代互查的闭包。先在同步事务内计算每个受影响 Run 的差量，再按直属 Issue 和祖先聚合差量。Issue 父边在产品中创建后不变。读取用量只读闭包及已保存的小计，不读原始记录和原生历史。
- 独立 `GET /api/v1/issues/{id}/token-usage` 使用现有 Bearer/Web Cookie 和数字 Issue ID 路径，返回 direct、subtree 及各节点直属数据，提供完全未观测的原因计数。Web 沿用 Issue 详情卡片和页内加载/重试。

目标运行时的原生来源可见 [Codex 0.172.0 通知转换](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/app-server/src/bespoke_event_handling.rs#L1666-L1689)、[同版本逐请求原件写入](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/core/src/session/mod.rs#L5872-L5914)、[上下文重新估算](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/core/src/session/mod.rs#L5998-L6041)、[上下文窗口已满标记](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/protocol/src/protocol.rs#L2671-L2695)。

## 验证计划

- Rust 定向验证身份、同键冲突撤回、迁移分母、混合顺序和重复重放；使用真实 Server/Runner 进程和受控原生通知构造父子树，覆盖首次同步响应丢失、重启、全量重复对账和迟到用量。回归 Issue detail、Run、SQLite 负载和协议窗口。
- SQLite WAL `synchronous=FULL`，约 2,000 Issues、20,000 Runs、500,000 请求。记录批量写入和事务分位数、每请求成本、DB 增长、每请求祖先写放大，对照逐条更新祖先的实现。记录 API/SQL 耗时和查询计划，确保没有请求历史扫描。
- Web 使用真实页面，验证 1440/390 的切换、未知/零、空状态、重试及不横向溢出。Rust 1.91.1 与 Node 24 / pnpm 10.34.7。最终候选独立审阅，直接推 main、等同 SHA Main CI，不部署。

## 读写契约与已知限制

- `tend issue token-usage <id|#number>` 与 Web 读取同一个端点。每个字段返回 `tokens`（十进制字符串或 `null`）和该字段的 `reported_requests`。字符串避免浏览器超过安全整数后静默取整。每套汇总还返回 `started_runs`、`observed_runs`、`unobserved_before_collection`、`unobserved_no_usage` 和有效原生请求数。未归属数据不挂到任一 Issue，上述原因计数只来自已验证的 Started。
- 同 Key 只保留第一份数据和一个冲突样例，冲突后不再计量该 Key。通知没有独立请求 ID，Runner 用本次追加的 `token_usage_record` 提供可核验身份，累计重置后两路无法精确匹配的快照不计量。旧版或不同宿主若不提供本机旧式 rollout 路径或逐请求原件，保留分母但不猜测用量。冲突隔离只作用于已核验身份的冲突，不把非请求快照误认为冲突。完整尾部没有可用证明，掉进程后未写入的通知没有原生历史回补。成功写入的记录才享有本地重传及对账保证。
- 已收到后停止或换 Runner 的原 Run 按历史 assignment 校验，不依赖当前 owner。前驱正常退场时，支持统计的 Server 必须先确认待传用量；与旧 Server 通信仍可以退场，尚未确认的用量留在原 Runner SQLite。Server 迁移 38 后旧 Server 二进制不能打开升级库，旧 Runner 仍可同步。覆盖初始化使用 37→38，保留之前的 handoff 和 PR 迁移。旧 Server 未确认时本机退役清理仍会拒绝删除含有待传用量的库。当前 Run 生命周期记录一个原生 turn。未来若需一个 Run 跨多个 turn，需单独扩展验证身份。
- 父关系沿现有只在创建时写入的 Issue 契约。覆盖迁移读取一次已接纳的 Run 事件并维护已持久的子树累计。日常页面打开时才读统计，读取的是该时刻已同步的快照（可手动更新）。超大子树的节点响应仍按 Issue 节点数线性增长，Web 先渲染 30 行。暂存待 Started 的事件在原 Started 到达时核验和清理，无对应 Started 的孤立暂存不会展示，也没有本期历史回收策略。

## 实测性能

容量对照使用临时 SQLite、release Rust 1.91.1、WAL / `synchronous=FULL`、2,000 个五叉树 Issue、20,000 个已验证 Run、每 Run 25 条原生用量（共 500,000 条），原生 response ID 为按实际样本长度构造的 58 字节合成键。每事务最多 32 条，两种实现都在同一事务大小下调用生产用量归并逻辑。基线逐条更新 Run、直属 Issue 及全部祖先。入库对照不包含 HTTP、Runner 本地写、启动迁移或其他同步操作；事务时间包含 SQLite 提交。行写操作是 SQLite `total_changes()` 的逻辑计数，不是文件系统写入量。

| 指标 | 按 Run / Issue / 祖先批量归并 | 每条请求更新祖先 |
| --- | ---: | ---: |
| 500,000 条总入库 | 47.87 s | 65.52 s |
| 平均每条 | 95.75 μs | 131.04 μs |
| 每同步事务 p50 / p95 / max | 2.29 / 6.72 / 76.13 ms | 3.81 / 5.98 / 28.51 ms |
| 事务体 p50 / p95 | 0.49 / 0.76 ms | 2.01 / 2.78 ms |
| COMMIT p50 / p95 | 1.76 / 6.06 ms | 1.68 / 3.61 ms |
| Issue 聚合行写入 / 请求 | 0.212 | 6.513 |
| 全部逻辑行写入 / 请求 | 1.282 | 8.513 |
| checkpoint 后文件大小 | 216,641,536 B | 216,641,536 B |

初始已建投影的库为 17,477,632 B，checkpoint 后增长 199,163,904 B（约 189.9 MiB）。按本次有局部性的请求顺序，Issue 聚合写入少约 **30.7 倍**，总耗时少约 **27%**。请求越分散，可在同一批共享的祖先和 Issue 越少。该次测量与隔离回归共享开发机，COMMIT 尾延迟样本不代表独立部署延迟，不能把这些倍数当固定吞吐承诺。两种写入方式验证同一根节点 500,000 条有效用量和 25,000,000 Token。

聚合读取对 UUID 路径只执行一条聚合 SQL，数字入口另通过短编号索引查一次 UUID。各路径暖库顺序请求 100 次。根子树响应 1,113,719 B，叶节点响应 1,564 B。

| 已保存投影 | Store 读 p50 / p95 | 真实回环 HTTP p50 / p95（UUID 路径） | HTTP p50 / p95（数字路径） |
| --- | ---: | ---: | ---: |
| 根节点，2,000 Issue | 10.08 / 13.52 ms | 44.67 / 51.69 ms | 44.50 / 52.19 ms |
| 叶节点，1 Issue | 0.009 / 0.021 ms | 0.85 / 1.04 ms | 0.95 / 1.19 ms |

```text
SEARCH e USING PRIMARY KEY (ancestor=?)
SEARCH i USING INDEX sqlite_autoindex_issues_1 (id=?)
SEARCH n USING COVERING INDEX sqlite_autoindex_issue_short_numbers_1 (issue_id=?)
SEARCH d USING PRIMARY KEY (issue_id=? AND scope=?) LEFT-JOIN
SEARCH s USING PRIMARY KEY (issue_id=? AND scope=?) LEFT-JOIN
USE TEMP B-TREE FOR ORDER BY
```

排序只对所选子树节点执行。查询没有访问 `token_requests`、`token_runs` 或 `token_usage_pending`，也没有扫描无关 Issue。`TEND_TOKEN_USAGE_BENCH_DB=/tmp/isolated.sqlite cargo +1.91.1 test --release --locked -p tend-server token_usage_scale -- --ignored --nocapture` 可另保存合成库供回环 HTTP 复测。不要指向实际业务库。

## 验证与真实渲染

- `cargo +1.91.1 fmt --check`、`cargo +1.91.1 test --locked`、`cargo +1.91.1 build --release --locked`。定向存储测试包括 Usage 先到、同批 Started/Usage、回执与原生身份去重、冲突撤回、错误来源排除、孙 Issue 聚合、仅迁移已接受 Started、沿前驱 Run 迟到回修。
- `python3 -B tests/e2e/issue_token_usage.py target/release/tend-server target/release/tend target/release/tend-runner --web` 使用真实进程和隔离的 App Server fixture。直属为 41,200，含两个子 Issue 为 127,200。三个已开始 Run 中两个观测到有效请求，一个完全未观测。真实 Runner 离线完成、同步响应丢失、重启重传以及两次全量对账后仍只有四条请求、不改变聚合。fixture 还测试启动响应前通知、错误 thread/turn 通知、压缩重估及窗口已满标记、Steer 响应前的正常用量、证据晚于通知但仍在活跃回合持久化、Stop interrupt 回复前的用量以及显式零与缺字段。冷启动和恢复的真实 Codex 0.172.0 均在回环 Responses fixture 中验证通知与本轮追加记录可匹配。真实 Codex、Tend Runner 和 Server 连通回环模型时，API 核对 102 Token / 1 请求。任务对账覆盖旧回复省略能力位、随后切换至 v38 并发送两页用量的场景，即使注入旧版的 scoped drained 信号也不会提前退场。Main CI 的 Rust job 运行不带 `--web` 的版本。
- 现有 `issue_detail.py`（真实页面）、`issue_run.py`、`storage_load.py`、`protocol_window.py`、`web_session.py`。Node 24.22.1 / pnpm 10.34.7 验证 `typecheck`、`lint`、`format:check`、`test`、`build`。Browser plugin 未提供，使用仓库的 Playwright Chromium。1440×1000、390×844 核对范围切换、503 重试及焦点、空/缺覆盖、显式零与大整数、无横向溢出和非预期 console error；实际数据来自合成隔离实例，不使用线上 #8。
- 渲染原件 [桌面](images/issue-token-usage-1440.png) · [手机](images/issue-token-usage-390.png) · [手机逐 Issue 明细](images/issue-token-usage-390-breakdown.png)。与 #8 原型相比，保留已上报总量、已观测分母和可解释未知，删去本期不做的资源页、按日期筛选及执行抽屉。
- 同机升级的本地归档回归在第二次收尾重试出现监听端口占用（另一次遇到宿主进程目录权限错误）。用 `15c5c3a` 的 Main CI 原始二进制和脚本同样复现端口失败。独立退役存储和后继补件 fixture 通过。最终候选的归档升级以同 SHA Main CI 为准。
