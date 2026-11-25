# Web 会话、入口与鉴权

Tend 是单用户系统。`TEND_TOKEN` 是唯一共享密钥，宜使用随机高熵值。浏览器仅在用户手动提交登录时发送 Bearer，其他 Web 请求使用同源 Cookie。CLI 和 Runner 无变化。

## 入口与寿命

Server 原生监听 HTTP。远程 Web 由同机可信 TLS 入口终止 TLS 并经回环转发，保留公开 Host。Server 不接受非回环上游的 Web 会话请求。用 `--web-origin https://tend.example.com` 配置唯一 Web 入口，非默认端口也是 origin 的一部分。只接受规范化的 HTTPS origin，不带路径、尾随斜杠或默认端口。Server 不根据转发头切换策略。HTTPS 入口不应把 HTTP 请求当成 HTTPS 转发。使用独立 hostname，不把同 host 不同端口当作浏览器 Cookie 隔离边界。Server 校验入口 Host，持久记录也绑定完整 origin；改变 origin 不继承登录。

未配置 HTTPS 入口时，只有来源地址为回环、请求 Host 为规范的 `localhost`、`127.0.0.1` 或 `[::1]` 的本机 HTTP 可以登录。回环代理仅供本机开发使用，不应向远端转发未加密的 Web 登录。普通远端 HTTP 页面只提示打开已配置的 HTTPS 地址，不渲染令牌表单，不读取 Web 会话或发送 Bearer。原有 Bearer 仍可用于受信任网络内的 CLI 和 Runner。 [配置示例](../README.md#web-sign-in-over-https)

| 模式 | Cookie | Server 绝对有效期 |
| --- | --- | --- |
| HTTPS 默认 | `__Host-tend-session`，不设 Max-Age | 12 小时 |
| HTTPS 记住 | `__Host-tend-session`，Max-Age=604800 | 7 天 |
| HTTP 本机回环 | `tend-loopback-session`，不设 Secure 或 Max-Age | 12 小时 |

全部模式使用 HttpOnly、SameSite=Lax、Path=/，不设 Domain。HTTPS 两种模式都有 Secure。不滑动续期。浏览器可能恢复普通会话 Cookie，仍受 Server 上限约束。Server 在 SQLite 按 ID 摘要、凭据代次、origin 和到期时间检查，重启保留有效会话。Server 用新 `TEND_TOKEN` 启动时拒绝并删除旧代次会话，即使以后切回原值也不恢复已删除的记录。接口和运维日志不得记录凭据或原始请求头。 [存储边界](storage.md#建议保存范围)

升级前备份 Server SQLite。迁移 30 后，旧 Server 版本不会打开新版本的数据库。

## HTTP 契约

`/api/v1/web/session` 响应不缓存，不提供跨源 CORS。

| 方法 | 请求 | 成功与错误 |
| --- | --- | --- |
| POST | 正确 Bearer、JSON `{"remember": false}`（或 `true`）、精确 Origin 和 `X-Tend-Web: 1` | 204 并签发新 Cookie。撤销此请求在同一入口出示的旧会话。错误或缺失 Bearer 为 401，入口或同源校验失败、HTTP 记住请求为 403。 |
| GET / HEAD | 当前 Web Cookie，不发送 Authorization | 有效为 204，无效或缺失为 401。只读，不回收过期记录、不更新到期时间、不清 Cookie。 |
| DELETE | 当前 Web Cookie、精确 Origin 和 `X-Tend-Web: 1`，不发送 Authorization | 撤销当前会话并清 Cookie，已失效同样返回 204。无同源证明返回 403。 |

普通 API 先检查显式 Authorization。存在但不合法即 401，绝不回退 Cookie。Cookie 只允许显式列出的 Chat 消息、Attention 和通知、Issue 读写（看板、详情、讨论、普通 Attention 回复及历史、启动、取消或重开）、只读资源目录（Agents 和 Runner 列表）、交付下载及 Server 的 rollout 读取。Cookie 写请求必须有匹配入口的 Origin 和 `X-Tend-Web: 1`。同步、上传、升级、原 Runner inspect 等运维接口只接受 Bearer。GET 和 HEAD 不改变会话或业务状态。

前端引导先读取会话状态。已登录 API 的 401 会结束当前前端会话、停止旧轮询并保留 URL，迟到的旧请求不结束新会话。403、Server 错误和断网不退出。退出成功后以 BroadcastChannel 通知同源标签取消旧检查并复查，尚未完成初始检查的标签也处理该通知。在途的手动登录尝试会被取消，迟到的会话确认不恢复登录或清除旧 key。没有 Web Locks 时仍遵循相同的取消规则。退化时使用无凭据 storage 事件与恢复焦点复查。旧 `tend.token.v1` 不参与自动登录，只有手动换取并确认 Cookie 可用后才清理本标签的旧 key。浏览器拒绝存储时显示恢复方法。

## 隔离验证

用 devcontainer 构建，先运行 Rust 工作区及 Web 检查。以下测试只用临时数据库、固定 App Server fixture 和 dummy token。省略 `--web` 可只验证 HTTP/SQLite/Bearer 边界（同主干 CI）。带 `--web` 时由测试进程创建本地 TLS 入口和临时 Chromium profile，不访问已部署服务。浏览器环境需有 OpenSSL、Playwright Chromium 和对应系统依赖。

```sh
cargo build --locked
pnpm --dir web install --frozen-lockfile
pnpm --dir web build
pnpm --dir web exec playwright install chromium
python3 -B tests/e2e/web_session.py target/debug/tend-server target/debug/tend target/debug/tend-runner \\
  --web web/dist --screenshots /tmp/tend-web-session-screenshots
```
