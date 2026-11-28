# Codex 原生会话与排障

Tend 托管的每个新会话都向 Codex App Server 请求持久化的原生 thread。受限 App Server 也走同样的持久化入口，权限、工具、cwd 和业务归属保持各自的规则。

| 执行入口 | 原件归属 | 恢复规则 |
| --- | --- | --- |
| 主 Chat Router，含使用原用户证据的受限转交 | Server 的绝对 `CODEX_HOME` | 按原输入和 attempt 查原 thread、turn。已有正常 Router thread 的接续不换 home。 |
| 普通讨论、草稿、快速转交总结、旧式隔离接收 | Discussion 配置的 `codex_home` | 普通讨论复用兼容 thread。总结使用登记的有界前沿或另起 thread。旧式隔离接收使用独立 thread，不写入普通 Router 的绑定。 |
| 飞书研判 | 研判配置的 `home/.codex-ark` | 每个研判 attempt 使用一个新 thread，且只进行隔离研判。排障从保存的原 attempt 读回，业务重试不在旧 thread 再跑。 |
| Runner 的 Issue、Work、Review Agent 和其原生子 Agent | 该 Run 的 launcher 使用的 `app_servers.<name>.codex_home` 或 Runner 的绝对 `CODEX_HOME` | 原 Run 恢复及按需读取使用原 launch 绑定。同 Task 的兼容且已停妥 thread 按原规则复用。同机替换继续保留原 home 与查找索引。 |

## 配置与保留

- 使用服务专用、权限受限且可写的持久目录。Router 不配置有效 `CODEX_HOME` 时 Server 拒绝启动。Runner 没有专用启动器 home 或服务 `CODEX_HOME` 时不会启动 App Server。没有默认回退到个人 `~/.codex`。启动器必须遵守传入的 `CODEX_HOME`。支持回报 `codexHome` 的 App Server 若回报其他目录，Tend 不会提交 turn。
- 专用 home 不能与个人 `~/.codex` 或其符号链接目标重叠。其下的 `sessions` 和 `archived_sessions` 必须是实际目录，不能是符号链接或其他文件。缺少时由 Codex 按需创建，启动与历史扫描使用同一布局。
- Discussion 必须与 Server Router 和 Feishu 的私有目录分开。飞书仍使用独立 `HOME`、空 cwd 和无原生副作用能力的研判 profile。只给相应服务账号和经授权的本机操作者访问私有 home。原件可能包含提示、飞书片段、模型输出和工具记录。
- 保留原 home 路径给后续查证，不把新的空目录当作原 home。配置换代、归档和清理前核对旧绑定与所需保留期限。飞书建议的七天业务保留不自动清除 Codex 原生会话。原生会话保留由所属 home 的本机运维负责。
- 不把 rollout 文件或其中的飞书研判上下文复制到业务库、仓库或 UI。既有飞书建议的短期证据、结构化研判结果、用户显式选定的引用和转交快照继续使用原业务契约。计量只存身份、路径绑定、模型和请求级计数。Runner 的既有按需查看接口仍从实际执行实例读原 turn，不归档 rollout 副本。 [存储边界](storage.md) · [飞书建议](plans/feishu-suggestions.md)

## 按原身份排障与计量

Server 运维从对应 attempt 定位 `thread` 和 `turn`。主 Router 和 Discussion 可用已登记的原路径。历史扫描先验证其原 home 布局，文件移动或归档后仍在同一个 home 查找，不改绑当前配置。飞书研判从 attempt 的 `codex_home` 在 `sessions` 或 `archived_sessions` 定位原 thread。Runner 按 Run 绑定查询。操作者可在原 home 启动**仅用于读取**的同配置 Codex App Server，初始化后调用 `thread/turns/list` 按原 thread 分页，核对原 turn 和终态。不需开新 turn、重新投递飞书或重跑模型。直接核对文件时比对 `session_meta.id` 和 `turn_context.turn_id`，不要只按时间或文件名判断。

本机只读查证可按已知 attempt ID 查询这些字段，无需读取正文。

```sql
SELECT a.source, a.thread, a.turn, a.rollout, f.codex_home, f.selected_model
FROM token_agent_attempts a
LEFT JOIN token_feishu_attempt_launch f ON f.attempt = a.id
WHERE a.id = :attempt_id;
```

飞书登记 `thread/start` 回报的此次选定模型。用量只在取得原生 response ID 后沿同一个 attempt、thread 和 turn 归集。仅当原生模型未知时，用量汇总以启动选择作回退，不将其写成逐请求的原生证据。原生回合、请求或实际响应模型优先，明确冲突保持未知，不从当前配置补猜旧记录。显式全量 `tend token-usage backfill --all` 会扫描新飞书 attempt 的原 home。旧无原件尝试报告 `ephemeral_no_history`，已登记持久 home 却找不到文件报告 `source_missing`。实时已保存的计数不因文件缺失而移除。 [系统来源用量](plans/system-agent-token-usage.md)
