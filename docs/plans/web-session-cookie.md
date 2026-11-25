# Web 会话 Cookie 持久登录实施计划

- 目标：保留单用户和共享 `TEND_TOKEN`。浏览器只在明确提交登录表单时用 Bearer 换取服务端会话，默认 12 小时绝对过期，仅显式勾选且通过配置的 HTTPS 入口才记住 7 天。不做滑动续期。
- Server：在 SQLite 迁移中加入会话表，随机 256 位会话 ID 只通过 HttpOnly Cookie 传输，持久化 SHA-256 摘要、凭据代次、入口和到期时间。签发时轮换同一浏览器出示的旧会话，退出撤销当前会话，启动时淘汰过期及旧凭据代次。GET 不清除或续期会话，避免迟到的 401 覆盖新 Cookie。
- 入口：显式配置一个可信 HTTPS origin，校验回环上游与 Host，不读取客户端转发头。配置后仅该入口允许 Web Cookie。未配置时只有本机 loopback HTTP 请求可用非持久 Cookie，远端 HTTP 保留 Bearer API，不开放 Web Cookie 登录。持久 Cookie 使用 `__Host-`、Secure、SameSite=Lax、HttpOnly、Path=/，不设 Domain。Cookie 路由按方法和路径允许，所有 Cookie 写请求必须有完全一致的 Origin 和 `X-Tend-Web: 1`。显式 Bearer 优先且失败不回退。Runner 同步、inspect、运维与 CLI 保持原鉴权。
- Web：启动检查会话，移除组件中的 token 传递，统一处理当前会话的 401、取消旧请求和轮询、保留路由。退出成功后通过同源无凭据通知刷新其它标签的会话状态。旧 key 仅在用户手动登录且确认 Cookie 可用后移除，存储受限时给可恢复提示。
- 验证：覆盖 SQLite 持久化、过期、轮换、撤销、Cookie 属性、CSRF、路由与 Bearer 兼容。使用临时 SQLite、回环 HTTP 和测试 HTTPS、dummy token 以及 Playwright 验证完整流程，附最终页面截图。运行 Rust/前端检查及既有受影响的端到端 fixture，对不可变候选独立审阅并直接推送 main。不部署或触碰运行中的服务。

## 独立审阅后的修订

- 远端 HTTP 在浏览器一侧先显示 HTTPS 地址提示，不渲染令牌表单、不检查会话，也不允许登录请求函数发出令牌。保留三个明确的本机 HTTP 回环主机与 HTTPS。用隔离页面断言远端 HTTP 没有登录 POST 或 Authorization。
- 同源退出通知取消旧会话检查并重新读取 Server，包括尚未完成初始检查的标签。用隔离 Chromium 延后真实的初始 204，验证退出后复查 401、保持原路由，迟到的 204 不恢复 UI。
- 手动登录的令牌交换与会话确认同样受同源退出取消。确认响应必须在原尝试尚有效时才能进入工作台或清除旧 key。隔离 Chromium 禁用 Web Locks，分别验证 BroadcastChannel 和 storage 回退都会取消在途确认，迟到的 204 不改变原路由和登录状态，随后可以手动重试。

## 与 Issue 详情回复的集成

- 保留主线的直接 Attention 回复迁移 29，Web 会话使用迁移 30。验证 29→30 不重建已有回复表，旧版迁移与重启仍可用。
- Issue 页面回复和分页历史使用统一会话客户端，服务端仅分别开放带同源校验的 POST 和只读 GET。保留 CLI Bearer、原 Chat 的 Attention 状态、应答幂等及存储不可用时的恢复提示。
