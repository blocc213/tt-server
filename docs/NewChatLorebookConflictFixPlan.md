# Server 模式无法新建聊天：世界书冲突命令缺失修复方案

## 1. 目标与结论

用户报告：点击新聊天，页面提示：

```text
新聊天已取消
Not found: Command 'check_character_lorebook_conflict' is not available in server mode
```

根因：Server 宿主没有暴露前端已经使用的世界书冲突命令；不是聊天文件损坏，也不是据此就能判断角色卡存在世界书冲突。

必须成对接入：

- `check_character_lorebook_conflict`
- `resolve_character_lorebook_conflict`

仅补检查命令会让无冲突角色恢复，但有冲突的角色点击解决选项后仍遇到第二个 404。

本次只调查并编写方案，没有修改业务代码、访问朋友的服务、创建/删除聊天或部署。朋友报告的错误作为事实接受；未重复点击其新聊天按钮来确认。

## 2. 已核对的代码证据

### 2.1 新聊天的失败链路

`src/script.js`（本次查看的 tt 源码行号，实施以函数名为准）：

1. `doNewChat()`，约 12545 行，在 `clearChat()` 之前调用 `resolveCharacterLorebookConflictBeforeNewChat()`。
2. 检查函数约 12451 行：先 `flushWorldInfoSaves('new_chat_lorebook_conflict_check')`，再调用 `getCharacterLorebookConflict(character.avatar)`。
3. 请求 `POST /api/characters/lorebook-conflict`，body 为 `{ avatar_url }`。
4. `src/tauri/main/routes/character-routes.js:144-160` 解析 avatar 身份，调用：
   ```js
   context.safeInvoke('check_character_lorebook_conflict', {
       dto: { name: characterId },
   });
   ```
5. Server 缺少命令，404 经错误边界传回；`readLorebookConflictResponse()` 保留错误内容与 HTTP status。
6. 检查函数 catch 后提示“新聊天已取消”，返回 false；`doNewChat()` 就此退出。

注意：前端不会因为角色没有嵌入世界书就跳过 RPC。普通单角色聊天，只要选中有效角色，都可能被缺失命令拦住。原生群聊在这个检查函数入口被跳过，此方案不等同于修复原生群聊全部功能。

### 2.2 第二个必须补齐的命令

`character-routes.js:163-194` 的 `/api/characters/resolve-lorebook-conflict`：

```js
context.safeInvoke('resolve_character_lorebook_conflict', {
    dto: { name: characterId, resolution, conflict_token: conflictToken || null },
});
```

完成后刷新角色缓存。该命令还被角色导入/替换之后的 `resolveImportedCharacterLorebookConflict()` 间接使用，因此不要把修复写成只针对“新聊天按钮”的特判。

### 2.3 两份 Server 源码均缺少命令

已搜索：

- `/home/admin/tt/src-tauri/crates/tauritavern-server/src/rpc.rs`
- `/home/admin/tt-github/src-tauri/crates/tauritavern-server/src/rpc.rs`

两份的 `EXPOSED_COMMANDS` 和 `run()` 均没有上述两个命令。

桌面版已有正确包装：
`src-tauri/crates/tauritavern/src/presentation/commands/character_commands.rs:120-152`。

共享实现已经存在：
`tt-application/src/services/character_service.rs`：

- `check_lorebook_conflict(dto)`
- `resolve_lorebook_conflict(dto)`

服务端 `ServerServices.character_service` 已装配，包括角色仓储和世界书仓储。本次无需增加服务、端口、仓储、依赖或修改 composition。

### 2.4 后续依赖

这条单角色链路使用的以下命令/路由已在本机源码中接通：

- 世界书读取：`/api/worldinfo/get` 经 `world-info-broker.js` 调用 `get_world_infos_batch`，不是缺失的单条 `get_world_info` 命令。
- 世界书列表刷新：`get_sillytavern_settings`。
- 角色缓存刷新：`get_all_characters`。
- 新聊天角色元数据持久化：`update_character_card_data`。
- Server 模式聊天读写：`/api/chats/get` 与 `/api/chats/save`。

朋友部署的具体版本未核验，实施时须在其实际构建源码确认这些后续路径；不要根据本机源码就声称朋友端已端到端通过。

## 3. 最小实现

### 3.1 只补 Server 分发

主修改文件：
`src-tauri/crates/tauritavern-server/src/rpc.rs`

在 `EXPOSED_COMMANDS` 的 characters 区域增加两个名称；在 `run()` 同一区域增加：

```rust
"check_character_lorebook_conflict" => ok(state
    .services
    .character_service
    .check_lorebook_conflict(arg(&args, "dto")?)
    .await?),
"resolve_character_lorebook_conflict" => ok(state
    .services
    .character_service
    .resolve_lorebook_conflict(arg(&args, "dto")?)
    .await?),
```

这是参考代码，合并时遵循现有排序和格式；不要重写整份 rpc.rs。

两个 DTO 位于 `tt-application/src/dto/character_dto.rs:131-165`：

```json
{"dto":{"name":"角色文件身份"}}
```

```json
{"dto":{"name":"角色文件身份","resolution":"current","conflict_token":"检查结果中的 token"}}
```

- 外层是 `dto`，不要错误地把 `name` 放到顶层。
- `name` 是前端已经通过 avatar 解析的角色身份，不应再次替换为展示名。
- DTO 内字段仍是 `conflict_token`，不要因为顶层 invoke 别名规则就改成 `conflictToken`。
- resolution 保持枚举 wire 值 `current` / `embedded` / `copy`。
- 直接返回现有 DTO 序列化结果，不改为 `{ ok: true }`、`{ data: ... }` 或默认空对象。

### 3.2 必须保持的语义

检查结果字段：
`conflict`、`world`、`embedded_name`、`current_available`、`conflict_token`。

解决结果字段：
`world`、`affected_world`、`world_written`。

现有业务行为：

| 情况/选择 | 正确行为 |
| --- | --- |
| 无嵌入书，或没有 world 绑定 | `conflict: false`，继续新聊天 |
| 嵌入书和绑定本地书一致 | `conflict: false` |
| 内容不一致 | 返回冲突详情和 token，由用户选择 |
| 绑定本地书缺失 | `conflict: true, current_available: false`；新聊天界面可恢复嵌入书或取消 |
| `current` | 用本地书更新角色卡的嵌入副本，不覆盖本地书；`affected_world: null, world_written: false` |
| `embedded` | 用角色卡嵌入书覆盖/恢复绑定的本地书；返回受影响书名，`world_written: true` |
| `copy` | 用于导入/替换流程；另存嵌入版本，现有版本及绑定按共享服务既定规则处理，不强行改成 embedded |
| token 过期 | `ApplicationError::Conflict` → HTTP 409，前端重新检查并提示，不继续覆盖 |
| 用户取消 | 不新建、不清空旧聊天，不写入角色卡/世界书 |

兼容细节：当前共享服务允许旧调用方的 `current` / `embedded` 省略 token；`copy` 必须带 token。新聊天 UI 本身要求 token。此次仅接通命令，不擅自修改这个既有兼容契约。

错误用 `?` 传播，复用 `ServerError::from(ApplicationError)`：验证错误 400、真正找不到角色/资源 404、冲突 409、内部错误 500。不要捕获所有错误并返回 `conflict: false`。

### 3.3 不要这样修

- 不删除新聊天前的检查，不在 Server 模式直接返回 true。
- 不伪造空结果或“无冲突”以绕过报错。
- 不默认选择嵌入世界书覆盖本地版本。
- 不把 404 全部解释为角色不存在；命令缺失和数据不存在是不同问题。
- 不复制桌面宿主代码/引入 Tauri 依赖；直接复用现有 CharacterService。
- 不修改世界书比较算法、数据格式、提示词、token 规则或小白X扩展。
- 不借机实现自动暴露全部 Tauri 命令；维持显式白名单边界。

## 4. 回归检查

### 4.1 本次已经执行的调查检查

```sh
node --test tests/character-lorebook-conflict-routes-contract.test.mjs
```

结果：5/5 通过。这证明已有前端路由测试不会发现这次 Server 漏注册问题，因为测试中的 safeInvoke 是替代实现，不经过真实 Rust dispatch。此结果不是修复后验收。

### 4.2 新增最小 Server 回归

在 `rpc.rs` 现有测试模块中，复用现有隔离临时目录与 `composition::build`，必须经过 `dispatch()`，不能只检测白名单字符串或 mock CharacterService。

建议两个行为场景：

1. **无冲突 + 实际解决**：构造有效临时角色，先检查无嵌入书/无冲突返回 false；再构造绑定本地世界书与嵌入书不同的状态，经真实 check 取 token，再经 resolve 调用，检查磁盘/仓储的角色卡与世界书内容符合选择，复查 conflict 变为 false。
2. **过期 token 不覆盖**：check 后修改本地书，再用旧 token resolve；必须得到 Conflict，并确认新版本未被覆盖。

能利用现有服务测试覆盖的分支无需复制整套。Server 测试主要防漏注册、错误 DTO、错方法调用和错误状态传播。

已有素材/测试在：
`src-tauri/crates/tauritavern/src/app/contract_tests/character.rs`：

- `character_service_lorebook_conflict_resolution_uses_current_or_embedded_source`
- `character_service_copy_resolution_preserves_current_and_reuses_identical_copy`
- `character_service_rejects_stale_lorebook_resolution`
- `character_service_copy_resolution_saves_without_binding_when_current_is_missing`
- `character_service_unbound_embedded_lorebook_is_not_a_conflict`

其中用 `character_png`、`character_card`、`world_info` 构造可控场景。不要使用会自动把嵌入书同步成当前版本的角色保存流程来“制造冲突”，否则 fixture 会把待测差异抹掉。不要把服务端依赖连到 Tauri crate 来复用测试 helper。

### 4.3 浏览器验收

在测试角色/隔离数据下运行，不对朋友的真实角色卡做试验性覆盖：

1. 普通无世界书角色：打开已有聊天，点击新聊天；进入新对话、旧对话仍可回到；刷新后新会话可重新打开。
2. 世界书一致角色：不弹冲突框，正常新建。
3. 有冲突角色：分别验证“保存当前世界书”和“用嵌入世界书覆盖”；选择含义与落盘内容一致，新聊天完成。
4. 点取消：当前会话及内容、角色卡和世界书不变。
5. 绑定本地世界书缺失：界面不提供不可用的 current 选择，embedded 可恢复后新建，取消保留原聊天。
6. 冲突弹框期间另一个页面修改世界书：旧 token 返回 409，重新检查；不静默覆盖新数据。
7. 导入/替换角色后的冲突入口也不再出现 resolve 命令 404；`copy` 分支保留两个版本，不冒充新聊天按钮里的选项。

至少保存第 1 项以及一个真实冲突解决分支的页面/网络证据。观察新聊天后续读写及角色更新，不以 check RPC 成功替代新聊天完成。

命令入口（按环境选择，报告实际执行范围）：

```sh
cargo test --manifest-path src-tauri/Cargo.toml -p tauritavern-server
node --test tests/character-lorebook-conflict-routes-contract.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml -p tauritavern character_service_lorebook
cargo test --manifest-path src-tauri/Cargo.toml -p tauritavern character_service_copy_resolution
cargo test --manifest-path src-tauri/Cargo.toml -p tauritavern character_service_rejects_stale_lorebook_resolution
node scripts/check-rust-crate-boundaries.mjs
pnpm run check
```

桌面测试需要 Tauri/GTK/WebKit 和仓库 default/资源目录；仅挂 src-tauri 容易缺资源。只需修改 Server 分发时，优先证明 Server 真实 dispatch 行为，不为本次创建复杂测试基础设施。

## 5. 同步、部署与边界

- 本计划放在 `/home/admin/tt-github/docs/NewChatLorebookConflictFixPlan.md`，便于直接交给维护 tt-github 的模型。
- 本机运行目录为 `/home/admin/tt`，其 Compose 构建上下文也是该目录。
- 两份仓库并非相同工作树，rpc.rs 有不同历史改动；将本补丁按小范围 hunks 同步，不能用整文件覆盖解决分歧。
- 用户朋友的环境需使用包含补丁的新镜像/程序。源码更改、网页刷新或重启旧镜像不会自动更新后端。
- 本机实施时部署命令为 `docker compose up -d --build tauritavern`；朋友若使用其他 Compose/image，按其现有发布方式更新。
- 保留 .env、docker-data、角色卡、世界书和聊天；不可用清空数据作为解决办法。
- 构建前检查磁盘剩余空间。本机前次完整构建/桌面验证曾因 Cargo 缓存占满磁盘导致容器无法启动；使用受控临时构建目录，结束后只删除本次测试生成的缓存，不盲目 prune 用户数据。
- 最终交付应写明：两个命令都已实现、目标源码/镜像版本、实际通过的测试和浏览器路径；未验证朋友部署就不要声称其问题已在现场验证消失。
