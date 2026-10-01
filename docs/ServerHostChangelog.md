# 服务器宿主（tauritavern-server）开发记录

本文记录把 TauriTavern 以纯浏览器 + HTTP 服务器形式运行（`src-tauri/crates/tauritavern-server`）的改造工作。上游 TauriTavern 只有桌面/移动宿主；服务器宿主是本分支新增的，复用同一套 application service 与 adapter，不另写业务逻辑。

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
- TT 与 SillyTavern 共有的差距：vector 系列、caption、统计、UUID 导入、Node 插件、多用户等。

## 验证方式

- `cargo test -p tauritavern-server`；`cargo test --workspace --exclude tauritavern`（桌面 crate 需 GTK）。
- `node --test tests/**/*.test.mjs`（含 `tests/server-route-bypass-contract.test.mjs`、`tests/chat-server-version-contract.test.mjs`）。
- 隔离实例冒烟：新编译的 server 二进制 + 临时数据目录 + 假 provider（慢流、中途错误、群聊存读与版本冲突、备份下载、`../` handle 被拒）。资源目录需要 `default/` 与 `frontend-templates/`（参见 `Dockerfile`）。
