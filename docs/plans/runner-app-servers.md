# 按具名 Agent 使用本机 App Server

同台 Runner 的 `issue-router` 和 `implementation-economy` 使用 DeepSeek 4.1 Flash / Max，通过 Ark Codex home 运行。其他 Agent 使用部署的默认 Codex home。两组 Agent 共享原有 slot、workspace 与 Runner 身份。

Runner 配置保存可选的本机 App Server 启动项。具名 Agent 上报启动项的逻辑名称。Server 选择具名 Agent，Runner 领取时把可执行文件、参数和独立 home 固定到原 assignment。本机启动、停止后查证和按需读取 rollout 均使用该记录。已有记录和未指定启动项的 Agent 沿默认入口执行。启动参数与 home 只保存在所属 Runner。

固定 fixture 使用同一个 Runner 执行两种启动配置，验证切换配置后仍从原 home 读回历史。部署前用实际 Ark App Server 发出 `deepseek-v4.1-flash` / `max` 请求，并确认原生注入工具回调。
