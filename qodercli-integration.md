# QoderCli Agent 接入文档

## 1. 背景

Codeg 是一个多智能体编码工作台，统一管理 Claude Code、Codex、Gemini、OpenClaw、Cline、Hermes 等 agent 的会话。本次将 **Qoder CLI** 作为新 agent 接入，使其能在 Codeg 中被发现、启动、管理会话并导入历史数据。

Qoder CLI 是 Qoder AI 推出的编码助手，支持 ACP（Agent Client Protocol）协议，可通过 `--acp` 参数启动 stdio 通信模式。

## 2. 接入范围

共修改 **15 个文件**，新增 **1 个文件**（parser），总计 **+558 / -13 行**。

### 2.1 后端 Rust（12 个文件）

| 文件 | 改动类型 | 说明 |
|------|----------|------|
| `src-tauri/src/models/agent.rs` | 修改 | `AgentType` 枚举新增 `QoderCli` + `Display` 实现 |
| `src-tauri/src/models/message.rs` | 修改 | `TurnRole` derive 添加 `PartialEq`（parser 比较需要）|
| `src-tauri/src/acp/registry.rs` | 修改 | 4 处注册：agent 列表、registry ID、反查、元数据（Npx 分发）|
| `src-tauri/src/acp/connection.rs` | 修改 | 协议适配（核心，见 3.1）|
| `src-tauri/src/commands/acp.rs` | 修改 | 配置路径、skill 存储、config cascade no-op |
| `src-tauri/src/commands/conversations.rs` | 修改 | 3 处 parser 注册 |
| `src-tauri/src/commands/experts.rs` | 修改 | `supported_agents()` 包含 QoderCli |
| `src-tauri/src/commands/mcp.rs` | 修改 | McpAppType + MCP 配置读取（no-op 写入）|
| `src-tauri/src/commands/project_boot.rs` | 修改 | 字符串映射 `"qoder-cli"` |
| `src-tauri/src/db/service/agent_setting_service.rs` | 修改 | 默认启用 |
| `src-tauri/src/db/service/import_service.rs` | 修改 | import 注册 parser |
| `src-tauri/src/parsers/mod.rs` | 修改 | `pub mod qoder` |
| **`src-tauri/src/parsers/qoder.rs`** | **新增（384 行）** | JSONL 会话 parser |

### 2.2 前端 TypeScript（2 个文件）

| 文件 | 改动类型 | 说明 |
|------|----------|------|
| `src/lib/types.ts` | 修改 | AgentType 联合类型 + 4 个常量表 |
| `src/components/agent-icon.tsx` | 修改 | SVG 图标组件 + MONO_ICONS 注册 |

## 3. 关键技术点

### 3.1 prompt vs blocks 字段名适配（核心难点）

**问题**：sacp 库的 `PromptRequest` 结构体序列化时使用 `blocks` 字段名，但 QoderCli ACP 实现期望 `prompt` 字段名。发送 `blocks` 会返回 `Invalid params: expected array, received undefined` 错误。

**方案**：在 `connection.rs` 的 `run_conversation_loop`（第 3042 行附近）根据 `agent_type` 条件分支：

```rust
let mut prompt_response: Pin<Box<dyn Future<Output = Result<Value, Error>> + Send>> =
    if agent_type == AgentType::QoderCli {
        // 构造带 "prompt" 字段的原始 JSON-RPC 请求
        let req = UntypedMessage::new("session/prompt", json!({
            "sessionId": sid.to_string(),
            "prompt": prompt_json
        }))?;
        Box::pin(cx2.send_request_to(Agent, req).block_task().await)
    } else {
        // 保持原有 PromptRequest（"blocks" 字段）不变
        let prompt_request = PromptRequest::new(sid.clone(), prompt_blocks);
        Box::pin(cx2.send_request_to(Agent, prompt_request).block_task().await)
    };
```

下游 `stopReason` 解析改为从 `serde_json::Value` 中提取，兼容两种响应格式：

```rust
let reason: StopReason = serde_json::from_value(
    raw_val.get("stopReason").or_else(|| raw_val.get("stop_reason"))
        .cloned().unwrap_or(serde_json::Value::Null),
).unwrap_or(StopReason::EndTurn);
```

**影响范围**：仅影响 QoderCli agent，其他 agent 的 prompt 发送路径完全不变。

### 3.2 QoderCli 协议特殊要求

通过实际测试 QoderCli v1.0.21 确认：

| 特性 | 说明 |
|------|------|
| `protocolVersion` | 必须为数字 `1`（不是字符串 `"1.0.0"`），使用 sacp `ProtocolVersion::LATEST` |
| `notifications/initialized` | 不支持，发送会返回 `Method not found` 错误 |
| `session/new` 必填 `mcpServers` | 即使为空数组也必须传，否则请求失败 |
| `session/prompt` 参数名 | 必须使用 `prompt` 字段，不是 `blocks` |
| 响应 `stopReason` | 可能为 `stopReason`（camelCase）或 `stop_reason`（snake_case）|

### 3.3 MCP 配置管理

QoderCli 自管理 `~/.qoder/settings.json` 中的 `mcpServers` 配置。Codeg 侧采用**只读策略**：

- **读取**：`read_qodercli_servers()` 从 `~/.qoder/settings.json` 解析 `mcpServers` 对象，归一化为 `BTreeMap<String, Value>`
- **写入（upsert/remove）**：no-op，不修改 QoderCli 的配置文件
- **展示**：通过 `scan_local_servers()` 聚合所有 agent 的 MCP server，在 UI 中展示来源为 QoderCli

### 3.4 会话历史导入（JSONL Parser）

QoderCli 的会话存储路径：`~/.qoder/projects/<encoded-workspace-path>/<session-id>.jsonl`

Parser 实现（`parsers/qoder.rs`，384 行）解析以下 JSONL entry 类型：

| entry type | 提取内容 |
|------------|----------|
| `runtime-config` | sessionId、model、contextWindow、时间戳 |
| `user` | 用户消息文本、tool_result blocks |
| `assistant` | thinking blocks、text blocks、tool_use blocks、stop_reason、model、duration_ms |

支持内容：
- 多轮对话重建（user → assistant 交替）
- tool_use / tool_result 配对
- thinking（扩展思考）块
- 会话标题提取（取第一条用户消息前 80 字符）
- 按时间倒序排列

### 3.5 Npx 分发配置

```rust
AgentDistribution::Npx {
    version: "1.0.20",
    package: "@qoder-ai/qodercli@1.0.20",
    cmd: "qodercli",
    args: &["--acp"],
    env: &[],
    node_required: None,
}
```

安装命令：`npm install -g @qoder-ai/qodercli`

### 3.6 配置与 Skill 存储

| 项目 | 路径 |
|------|------|
| 配置文件 | `~/.qoder/settings.json` |
| Skill 全局目录 | `~/.qoder/skills` |
| 会话目录 | `~/.qoder/projects/<encoded-path>/` |
| Auth | `qodercli login`（终端交互，codeg 不管理）|

## 4. 文件变更详情

### 4.1 `acp/connection.rs`（+70 行，核心适配）

```
+ build_new_session_request: QoderCli 始终传 mcpServers
+ build_load_session_request: 同上
+ run_connection: protocol_version 注释 + 简化日志
+ run_conversation_loop: prompt vs blocks 条件分支
+ stopReason 解析: 从 Value 中提取，兼容 camelCase/snake_case
```

### 4.2 `acp/registry.rs`（+16 行）

```
+ all_acp_agents(): QoderCli
+ registry_id_for(): "qodercli"
+ from_registry_id(): "qodercli" → QoderCli
+ get_agent_meta(): Npx 分发元数据
```

### 4.3 `commands/mcp.rs`（+53 行）

```
+ McpAppType::QoderCli 枚举
+ read_qodercli_servers(): 从 settings.json 读取 mcpServers
+ mcp_upsert_local_server: 包含 QoderCli（no-op）
+ mcp_remove_server: 包含 QoderCli（no-op）
+ scan_local_servers: 聚合 QoderCli MCP servers
+ read_servers_for_agent_type: QoderCli → read_qodercli_servers
+ upsert/remove_server_for_app: QoderCli → no-op
```

### 4.4 `commands/acp.rs`（+9 行）

```
+ agent_local_config_path: ~/.qoder/settings.json
+ skill_storage_spec: ~/.qoder/skills
+ cascade_update_agent_config: QoderCli → no-op
```

### 4.5 `parsers/qoder.rs`（+384 行，新增）

完整 JSONL parser，实现 `AgentParser` trait：
- `list_conversations()`: 遍历所有 `.jsonl` 文件，返回会话摘要
- `get_conversation()`: 按 session ID 查找并解析完整对话

### 4.6 前端文件

**`src/lib/types.ts`**（+5 行）：
```typescript
export type AgentType = ... | "qoder_cli"
export const ALL_AGENT_TYPES: AgentType[] = [..., "qoder_cli"]
export const AGENT_LABELS = { ..., qoder_cli: "Qoder CLI" }
export const AGENT_COLORS = { ..., qoder_cli: "bg-emerald-500" }
export const AGENT_DISPLAY_ORDER: AgentType[] = [..., "qoder_cli"]
```

**`src/components/agent-icon.tsx`**（+20 行）：
- `QoderCliMonoIcon` SVG 组件
- `MONO_ICONS` 注册 `qoder_cli: QoderCliMonoIcon`

## 5. 编译验证

```bash
# 编译检查
cargo check --no-default-features --bin codeg-server ✅

# Clippy 静态分析
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings ✅
```

## 6. 合并与兼容性

- 已合并 main 最新代码（fast-forward，无冲突）
- 所有改动通过 `match QoderCli` 条件分支隔离
- **对现有 agent 零影响**：不修改任何已有 agent 的逻辑路径
- `PromptRequest` 仅在 QoderCli 分支被绕过，其他 agent 完全走原路径

## 7. 已知限制与待办

| 项目 | 说明 |
|------|------|
| MCP 写入 | QoderCli 的 MCP server 增删为 no-op，不支持从 Codeg 侧管理 |
| Auth | 需用户手动 `qodercli login`，Codeg 不管理认证 |
| 前端 CRLF | `agent-icon.tsx` 有 Windows CRLF 行尾问题（335 个 prettier 错误），需 `npx prettier --write` |
| 前端 build | 静态导出构建验证待完成 |
| E2E 测试 | 无自动化集成测试，依赖手动验证 |
