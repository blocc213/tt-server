# 服务器宿主（tauritavern-server）开发记录

本文记录把 TauriTavern 以纯浏览器 + HTTP 服务器形式运行（`src-tauri/crates/tauritavern-server`）的改造工作。上游 TauriTavern 只有桌面/移动宿主；服务器宿主是本分支新增的，复用同一套 application service 与 adapter，不另写业务逻辑。

## 2.4 升级

- 以 upstream v2.4.0 的分块提交、原始文档字节与 reader/cold-swipe 架构为准；不恢复旧 `chat_payload_commit_*`、旧设置视图或 native picker 浏览器调用。
- 字节长度/SHA-256 聊天版本前置条件迁入共享 ChatCommit 层，保留新建、条件替换和显式强制覆盖的区别；冲突时保留浏览器未保存内容。
- HTTP 宿主适配 2.4 设置、世界书、扩展存储、文件 staging 与二进制读写协议；reader 资源按已认证客户端会话隔离，不能跨会话读取。
- 保留 opt-in 后台生成的页面关闭恢复、原聊天目标核对、停止取消与过期处理；保留带凭据脱敏的服务端 LLM 控制台日志。日志仍包含提示词和非流式回复，必须按私密数据保护。
- 浏览器保留设置按钮和后台生成开关；原生 Android 的前台服务/AI 通知生命周期完全沿用 upstream 2.4。原生窗口、LAN Sync、文件选择/系统交付与调试包不作为服务器 RPC 暴露。
- 浏览器迁移导入通过受限 staging，导出通过 HTTP 下载；保留 upstream 归档 artifact 的领取生命周期和密钥/暂存目录排除策略。
- 升级不带入生成 schemas、Gradle 缓存或本地私有调查计划；部署与用户数据不属于源码合并范围。
- 原生 Channel 依赖的 Agent 实时投影/运行交互与浏览器扩展工具回调在服务器界面明确提示不可用；不伪装空成功。MCP、SearXNG、vector 等无 Channel 的服务沿共享实现接入。
- 自定义网络端点使用 upstream 访问许可模型；服务器许可记录位于专用 `.server-security`，不包含于导出且归档导入拒绝该路径，避免从归档授予网络权限。
- 真实浏览器启动不注册 native graceful-exit 监听，但保留 pagehide/beforeunload 的持久化 flush。核心模板和扩展模板由 `read_frontend_template` / `read_frontend_extension_template` 共享模板服务读取；模板资源限定于前端目录，拒绝路径穿越与符号链接越界。
- 浏览器支持 `/backendlog`、`/llmlog`（`/apilog`）与设置页对应按钮：服务器在内存保留最近 800 条后端日志（与 stdout 同一过滤）和最近 `dev.llm_api_keep` 条 LLM 请求/响应（与控制台日志相同脱敏，重启即清空），前端以 1 s 轮询按 `id` 增量拉取代替 Tauri 事件。调试包导出仍不暴露。


## 一、基础架构

- `tauritavern-server`：基于 axum 的 HTTP 宿主。`rpc.rs` 维护 `EXPOSED_COMMANDS` 白名单，把浏览器调用分派给与桌面命令相同的 application service；`composition.rs` 组装服务；构建守卫禁止它依赖桌面 app-shell crate。
- 前端在每次调用时检测宿主（而不是模块加载时），服务器模式下经 HTTP 启动、通信。
- 服务器原生端点绕过页内路由；后改为**精确路径集合**，不再按 `/api/backends/chat-completions/` 前缀整体绕过（以前会让页内实现的 `/bias`、`/multimodal-models/workers_ai` 在服务器版 404）。
- 资源经 `HostResourceService` 提供；媒体、缩略图、导出、浏览器上传均已接入。
- 上传统一写入数据根下私有的 `.server-upload-staging`，所有接受服务器路径的命令只认 staging 内、经 canonicalize 校验的路径，防止读写任意服务器文件。
- 流式 chat completions：取消会传到上游；首包后每 15 s 向浏览器发 SSE keepalive；流式请求没有总时长上限。
- Docker 部署：`Dockerfile` + `compose.yaml`，`.env` 只需设置 `TAURITAVERN_PASSWORD`（见 `.env.example`）；`docker-data/` 与 `.env` 不进入镜像或 Git。

## 二、聊天保存与版本令牌

- 聊天保存带版本前置条件（CAS）：旧标签页不能覆盖新内容，冲突时保留页面并给出明确错误，保存失败持久可见。
- 兼容按原版 SillyTavern 协议调用 `POST /api/chats/save`、不带 `version` 的第三方插件（如 LittleWhiteBox 首次保存地图引用）：`src/scripts/tauri/chat/transport.js` 的 `adaptLegacyServerChatSave` 在服务器模式下，对目标为当前打开角色聊天、且本页已记录版本的非 force 保存补上已记录版本，成功后推进缓存。未加载版本前仍返回 409，旧版本仍被 400 integrity 拒绝。规则记录在 `docs/FrontendHostContract.md` §4.2。
- 群聊读写：新增 `/api/tauritavern/group-chats/get|save`，使用与单聊相同的版本令牌 CAS；`GroupChatRepository` 增加 `get_group_chat_payload_bytes`。

## 三、补齐的服务器命令

- 角色卡导入（含外部 URL 导入）、扩展存储、图片元数据文件夹、普通聊天处理命令、快速回复保存/删除。
- `normalize_world_info_name`：新建/改名/导入世界书前都会调用 `/api/worldinfo/sanitize-name`，服务器版此前未登记该命令。现接入已有的 `WorldInfoService::normalize_world_info_name`，校验失败返回 400。
- 新增服务：GroupChatService、AssetService、UserMediaService、ProviderMetadataService、StableDiffusionService、TranslateService、TtsService。
- 新增命令约 69 条：群组 CRUD；群聊列表/搜索/分页/删除/改名/导入/备份恢复/metadata/store；角色聊天 store/metadata/搜索/定位/分页；聊天备份列表/删除/恢复/`read_chat_backup`；`import_world_info`；`restore_preset`；persona 头像上传删除；用户图片库；assets 库；OpenRouter/NanoGPT/SiliconFlow/Workers AI 元数据；SD；翻译；TTS。
- 聊天备份下载：服务器模式改调 `read_chat_backup`（浏览器不能打开服务器路径）。
- Agent 模式所需的服务器命令；修复空消息导致的 agent 失败。
- 手机后台生成保护（可选开关，默认关闭）：开启时服务器继续接收并暂存回复，页面断线后可重连取回；关闭时走原路径。

## 四、备份与数据迁移

- `/api/users/backup`（`archive.rs`）：直接返回整用户 ZIP，复用 `FileDataArchiveExecutor`；只有允许密钥外露时才包含 secrets；临时文件即删。
  - handle 必须是单级、已存在的用户目录；**canonicalize 后父目录必须正好是数据根**，拒绝指向数据根外的符号链接、`_tauritavern`、`.server-upload-staging` 等。
  - `Content-Disposition` 使用纯 ASCII 安全的 `attachment; filename="..."`。
- 全量数据迁移导入/导出在服务器上接入 `DataArchiveService`（`start_import_data_archive`、`start_export_data_archive` 等异步 job）。服务器没有"下载文件夹"或分享面板，导出改为浏览器下载：`GET /api/tauritavern/data-migration/export/download?id=<jobId>`，打开归档后从句柄流式返回。
- 服务器 staging 位于 `_tauritavern/archive-imports|archive-exports` 与 `.server-upload-staging`，共享导出器（`tt-adapter-archive`）会跳过这些宿主 staging 目录，避免把上次导出/上传打包进新归档。

## 四之一、安全与设置面板修复

- 全量数据导出（桌面与服务器共用的导出器）默认不再包含 `<user>/secrets.json` 与 `backups/secrets_migration_*.json`，只有允许密钥外露时才打包。
- 上传 staging 拒绝路径中的任何符号链接，并要求 canonicalize 后仍位于 staging 根内；`/api/users/backup` 的用户目录同样拒绝符号链接 handle。
- 服务器模式设置面板不再调用仅原生宿主可用的 runtime paths；数据目录、托盘、LAN Sync、后端/LLM 日志查看器、调试包等原生专属能力在服务器模式下隐藏或给出明确提示，而不是报错。
- 聊天备份存储统计（`get_chat_backup_storage_stats`）通过 RPC 暴露给服务器模式。

## 五、错误传播

- `src/scripts/openai.js` `tryParseStreamingError`：原实现在自己的 try 里 `throw` 又被同一 catch 吞掉，SSE 中的 `{"error": ...}` 只弹提示，流照常"成功"结束，留下空回复。现在只捕获 JSON 解析失败，`error` / `message`（字符串）/ `detail` 真正抛给调用方；`quiet` 只关闭提示，不吞异常。
- 当 `error` 字段本身是字符串（而不是 `{ message }` 对象）时，直接使用该字符串作为错误信息，不再显示为空或 `[object Object]`。

## 六、关于"长时间生成后请求超时 / 空回复"

排查结论：**TT 本身不是原因。** 在模型 API 前面放了一层反向代理（如 Nginx / Nginx Proxy Manager）时，nginx 默认 `proxy_read_timeout 90s`。模型在首包后长时间思考、不输出字节，超过 90 s 后代理断开上游连接，上游视为客户端离开，TT 收到被截断的流（表现为请求超时或空回复，上游可能仍计费）。

隔离验证：用假 provider 在首包后静默 100 s 再续流，TT 完整转发内容并以 `[DONE]` 结束。

处理方式（二选一）：

1. TT 的 API 地址直接连接模型 API（不经过该反向代理）。
2. 在反向代理上提高超时，例如：

   ```nginx
   proxy_read_timeout 600s;
   proxy_send_timeout 600s;
   proxy_buffering off;
   ```

## 七、未实现

- 原生专属能力：iOS picker/分享、系统通知、LAN Sync、桌面开发日志订阅——浏览器无对应能力。
- 统计、UUID 导入、Node 插件、多用户等仍非服务器宿主能力。2.4 的 vector 共享服务已接入；此处旧版 vector 差距不再适用。

## 验证方式

- `cargo test -p tauritavern-server`；`cargo test --workspace --exclude tauritavern`（桌面 crate 需 GTK）。
- `node --experimental-vm-modules --test --test-concurrency=2 --test-timeout=120000 --test-force-exit 'tests/**/*.test.mjs'`（含浏览器、版本 CAS、后台恢复与路由契约）。
- 隔离实例冒烟：新编译的 server 二进制 + 临时数据目录 + 假 provider（慢流、中途错误、群聊存读与版本冲突、备份下载、`../` handle 被拒）。资源目录需要 `default/` 与 `frontend-templates/`（参见 `Dockerfile`）。

### 2.4 升级验收记录

- 独立 `CARGO_TARGET_DIR`：服务器测试 43 项、改动的六个共享 crate 共 701 项通过；crate boundary 检查通过。未构建 GTK 桌面程序。
- 完整 Node 测试 534 项：533 通过，1 项平台测试跳过；UI 测试 191 项通过；`pnpm run server:build` 的前端与 release server 构建通过。
- 全新一次性数据目录、loopback 8013 与本地假 OpenAI provider 8014：HTTP 验证 bootstrap、设置读写、建角色、聊天保存/重读、swipe、旧版本 409、世界书、扩展存储、上传 staging、流式回复、后台断线重连和 ZIP 备份下载通过。
- Chromium 实际页面验证登录启动、设置按钮、保存立即刷新、发送流式聊天、swipe 后保存与刷新、元数据保存、冷 swipe 设置，以及关闭页面后重新打开只恢复一次回复并清理 job 记录。
- 明确禁用的 Channel 命令仅为 `subscribe_agent_run_live_projection`、`register_agent_extension_tool`、`set_agent_extension_tool_enabled`、`resolve_agent_extension_tool_call`；相关实时 Agent / in-app 工具界面显示服务器宿主不可用。无 Channel 的 Agent history/session/profile、MCP、SearXNG 与 vector 接共享服务。
- 验证实例均已停止；未改生产数据、生产源码或生产进程。日志含提示词的测试仅使用虚构内容。
