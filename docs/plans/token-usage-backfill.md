# 历史 Token 用量回溯与重建

本文记录 Runner Run 的回溯和重建契约。全量作业同时包含已留存的 Server 原生回合，见 [全系统 Agent 请求用量](system-agent-token-usage.md#历史与覆盖)。

## 命令与范围

- `tend token-usage backfill --all` 或指定一个 `--run ID`、`--task ID`、`--issue ID|#编号`（含子树）、`--runner ID`、`--host ID`。选择的是受理时 Server 已验证原生 Started 的 Run，Runner 和 Host 按原派发筛选。
- `--policy quarantine|prefer-native|prefer-server`。默认补缺并隔离不同值，不替换原值、不删除未观测的副本。可传 `--job ID` 重试同一创建请求。新 ID 表示一次新扫描。
- `tend token-usage status JOB [--after CURSOR]` 分页列 Run 与覆盖状态，`tend token-usage audit JOB [--after CURSOR]` 分页列异常证据和决议。
- `tend token-usage rebuild <范围>` 仅从 Server 已保存请求重建。全部写入口走 Server Bearer API，本地 CLI 不访问库。普通 Issue GET 仍走闭包和持久投影。

## 回传与覆盖

Server 持久保存作业和 Run 明细，复用已有 Run 原归属及正式退役的同 Host 后继查找。普通 RunnerSync 用有能力才启用的可选字段下发当前工作与回传有界页。旧代及旧协议缺省行为保持。Runner 在独立线程读取原 Run 本地绑定或同 Host 历史索引的冻结 Codex home，不启动 Codex，不向 Server 发送原件或路径。只定位该 thread 的 sessions/archived_sessions 原件，并按 thread/turn/response_id 读取原生逐请求记录。append 文件按一次有界快照读，批次用快照身份和推进位置防重。再执行一次作业以纳入受理后新 Started 或读取后发生的 append。

未同步、旧 Runner 不支持、离线、原绑定、原文件缺失、读取失败、文件损坏和无逐请求证据分别可见。运行中回合只证明扫描到的前缀，不推断最终覆盖。Server 独有键保留并报告，绝不把缺失当作零。原生计量与实时共用非负可选字段和 response 身份校验，只使用请求的 usage，不使用累计值、估计和历史恢复快照。

## 冲突与聚合

沿原 Run/assignment/task/thread/turn 和同 Host 读取权限校验，唯一键仍是 thread/turn/response_id。相同证据合并。若实时只上报原生记录的相同字段子集，使用原生可选字段补全并记录审计。原生证据在两次作业之间变值也是冲突，即使没有实时副本。不同值保存原值、原生值、来源、作业、策略和决议。原件内部冲突及可定位的无效记录不选择原生值。默认隔离使冲突不进入聚合，显式策略可选择有效原件或已有 Server 副本。若 Server 发现不同 Started Run 引用同一原生 thread/turn，整项标记 `ambiguous_turn`，不归入任一 Run。

批次在与实时观察相同的 SQLite 写入边界提交，请求事实、审计、投影和批次推进原子。重建从范围内非隔离请求重计 Run，再重计相关 Issue direct 和祖先 subtree，不变更原始证据和 Started 分母。不在 GET、启动迁移或后台定时触发。

## 验证

Rust 定向覆盖原归属、重复与乱序、隔离/纠正、实时交错、缺源、legacy 与确定性重建。隔离真实进程 fixture 以 277 Started、7422 原生请求、7417 双源一致为基线，覆盖不同来源差项、重复全量、实时交错、同 Host 退役接替和投影读。记录 SQL 查询计划及回环 HTTP 测量。运行 Rust fmt、全量测试、release build 和两条用量 e2e，并核对提交的 Main CI。不部署，不读取或修复生产原件。

## 运维与读回

- 回溯开始的 POST 返回 `202` 及第一页状态。以同一个 Job ID 和同一选择器、策略重试时返回同一作业，不重选 Run。相同 ID 及不同请求返回 `409`。选择器找不到已验证 Started 时返回 `selected_runs: 0`，不是零 Token 的证据。
- 状态行报告 `pending`、`reader_offline`、`reader_unsupported`、`active_prefix`、`gap` 或 `scanned`。离线和不支持的项仍等待 Runner 连接或替换。终态缺口保留在本次作业内，原件补齐后以新 Job ID 重扫。分页使用返回的 `next_after`。状态是当时扫描结论，后续实时证据变化出现在审计，不改写扫描结论。
- `prefer-native` 只选择本次快照中有效且单一的原生值。`prefer-server` 只选择有效且不含 Server 内部矛盾的已有值。无可选值时仍隔离。原生证据不变的默认重扫不撤销已经显式指定的策略。新冲突、后续实时变体和最终选择均有审计；原生内部冲突保留可解码的一对证据作为隔离依据，不传输原 JSONL 或无效任意字符串。
- Runner 最多处理一个 Run 的快照，单页最多 32 个响应键。同一 Codex home 的文件定位索引在每个作业内复用；共享一个 thread 的多个 Run 可能重读同一文件。多页扫描重启后可按同一快照继续，内容改变会记录缺口并保留已提交页。Job 状态和审计独立于页面路径，仅 Bearer API 可访问。
- 重建既不需要 Runner 在线也不将未捕获的请求补成零。部分重建从请求重计选中 Run，将其他 Run 已保存的小计纳入相关 Issue 重算；修复大范围漂移时使用 `--all`。有界写批次之间允许普通写者推进，WAL 读者可继续看上一完整投影。活跃写入会增加追平轮次，影子需要一份额外的投影空间，读取阶段可能推迟 WAL checkpoint。
- Server 已升级到 schema 44 后，定向重建也需要解码未重算 Issue 的直属投影，才能精确发布全局用量。若范围外的直属投影已损坏而无法解码，定向重建会报错且不会发布半成品。确认原始请求证据仍可读后，使用 `tend token-usage rebuild --all` 从 Server 已保存的请求重建全部投影。此命令不是回溯，不读取 rollout。

## 独立审阅修订与重建并发验收

- 同一 thread/turn 后到第二条已接受 Started 时，持久标记归属歧义，把受影响作业明细改为缺口，并在同一事务撤出此前由回溯引入的贡献，恢复仍有证据的实时副本。已有原件、请求级证据和历史审计不删除。相容实时副本按已知字段并集，无法举证的旧 conflict 和真正不同值继续隔离。
- 普通 JSONL 长行用流式读法识别类型并跳过无关 payload。超长且可能含用量的行单独报告缺口。
- 使用 WAL 快照在写事务外读取事实，写者用数据库触发器将改动 Run 放入按 Run 合并的脏队列。影子 Run 小计和 Issue 投影在有界批次中写入。对脏 Run 在新读快照重计并仅确认读到的版本。短事务检查脏队列为空、切换两张投影表，并一同发布从直属影子行汇总的全局用量。读到旧或新完整版本，不会读到影子半成品。仅显式操作启动，重启废弃未发布影子，从头重试即可。
- 复用 #100 的 2,000 Issue / 20,000 Run / 500,000 请求合成数据，真实回环 Server 进程在重建期间接受 Chat 收件和正常 RunnerSync。记录延迟、写事务占用、精确投影与重传验证，不设额外 SLA。

隔离复跑可在 release 测试里用 `TEND_TOKEN_USAGE_BENCH_DB=/tmp/isolated.sqlite` 保存 `token_usage_scale` 的合成库，再运行 `python3 -B tests/e2e/token_usage_rebuild_load.py target/release/tend-server target/release/tend /tmp/isolated.sqlite --shadow --branches`。脚本只用临时副本，打印重建期间真实回环 Chat / RunnerSync / 页面 GET 的样本及重建各阶段的写锁占用，并将最终读投影与唯一、非隔离的持久请求数对照。

撤销歧义归属时把仅来自回溯的键从计量副本移出，保留决议和历史证据。后续实时记录按原规则认定归属，已有可信实时副本的键仍归原 Run。定向重建只解码受影响投影所需的 Run 小计，其余 Run 和 Issue 投影原样搬移。全局汇总额外解码所有直属 Issue 投影，不重计未选中的 Run。追平的 Run、Issue 和版本确认分别有界提交，所有影子写入完成后再按捕获版本确认；跨多个子树的用例验证单批不会因扇出超过 128 行。
