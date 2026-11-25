# Shuttle

[English](./README.md) | 简体中文

**Shuttle** 是独立的本机优先 Rust 后台服务，通过可恢复任务队列连接 GitHub Issue / PR 与本机编程 Agent，并回写结构化状态评论与草稿 PR。与 Loom 推理网关及额度工具分别运行。

- **无需公网回调或上传密钥**：空闲时每 60 秒通过本机 `gh` 轮询 GitHub，单任务串行执行，无需公网 Webhook、GitHub Actions runner 或将 Agent Key 上传到 GitHub。
- **显式授权与安全边界**：标签和评论的手动派发仅接受仓库 `write`/`maintain`/`admin` 成员；PR 任务绑定精确的 40 位 head SHA，正文或 head 变更时自动取消旧任务。
- **控制器托管发布**：Agent 不接触 GitHub Token。控制器通过 GitHub Git Data API 将校验后的文本变更写入新的 `shuttle/*` 分支并创建草稿 PR，发布前自动执行路径逃逸与常见凭据扫描。

## 快速开始

```bash
cargo build --release --locked
./target/release/shuttle init --repo owner/repo --agent pi
./target/release/shuttle policy --agent pi --model hub/deepseek/deepseek-flash --auto-fix true
./target/release/shuttle doctor
./target/release/shuttle bootstrap
./target/release/shuttle install
```

`init` 在新机器默认只处理维护者派发；通过 `policy --auto-fix true` 明确启用自动修复。切换默认 Agent / 模型或改为仅自动分析：

```bash
shuttle policy --agent pi --model hub/deepseek/deepseek-flash
shuttle policy --agent codex --model auto
shuttle policy --agent opencode2 --model hub/deepseek/deepseek-flash

# 改为仅自动分析
shuttle policy --auto-fix false --auto-triage true
```

电脑关机或休眠时暂停处理，恢复后重新检查当前打开的 Issue / PR（基于 [GitHub REST Issues 接口](https://docs.github.com/en/rest/issues/issues#list-repository-issues)，包含 PR）。修复结果始终为草稿 PR，由维护者决定合并，且不改变个人 `pi` 的全局默认配置。

## 在 GitHub 派发

| 操作 | 行为 |
| --- | --- |
| `agent:codex` / `agent:antigravity` / `agent:pi` / `agent:opencode2` / `agent:grok` | 选择一个 Agent 分析（需恰好一个路由标签） |
| Issue 另加 `agent:fix` | 批准该 Agent 修复并产生草稿 PR |
| 评论 `/agent pi analyze` | 指定 Agent 分析 Issue / PR |
| Issue 评论 `/agent codex fix` | 批准修复 Issue |
| PR 评论 `/agent codex fix <完整的 40 位 head SHA>` | 批准修复该精确版本；生成新的草稿 PR，不改原作者分支 |
| `agent:paused` | 暂停该条目的新任务，并阻止当前任务发布 |

标签和评论的手动派发仅接受仓库 `write` / `maintain` / `admin` 用户；作者身份或标签文字本身不构成授权。启用 `auto_fix` 后，仓库允许列表构成对新条目的固定自动处理策略，正文不能覆盖 Agent、模型、工具或权限。服务自身创建的草稿 PR 不会自动触发自己，但接受维护者的明确派发。

## 本机查看与恢复

```bash
shuttle tasks
shuttle report 1
shuttle retry 1
shuttle publish 1
shuttle policy --agent pi
shuttle policy --auto-fix false --auto-triage false
```

- `scan`：仅发现并保存任务，不执行 Agent，也不向 GitHub 写评论。
- `once` / `listen`：执行单轮轮询或持续监听。
- `retry` / `publish`：`retry` 重新执行 Agent（可能再次消耗模型用量）；`publish` 仅恢复已有补丁的 GitHub 发布，不再次调用模型。发布阶段网络失败最多自动恢复 5 次（`publication_pending`），生成阶段不会自动重跑。
- 状态集合：`queued`、`running`、`publishing`、`publication_pending`、`completed`、`failed`、`blocked`、`interrupted`、`cancelled`。单任务默认限时 30 分钟；详细分析结论通过 `shuttle report <任务号>` 在本机查看。
- 滚动 24 小时自动分析 / 修复上限默认 20 次（可通过配置 `auto_daily_limit` 调整为 `1–100`），超限任务标为 `blocked`，不自动换模型或静默重试。

## Agent 适配

- **Codex**：使用官方 [`codex exec`](https://developers.openai.com/cookbook/examples/codex/build_iterative_repair_loops_with_codex)，分析为 `read-only`，修复为 `workspace-write`，忽略全局执行覆盖并保留本机认证。
- **pi**：禁用全部内置工具、MCP、Skills、项目配置与扩展发现，仅加载控制器提供的 `relay_file` 文件工具（分析允许 `list`/`read`，修复增加 `write`/`remove`，无 bash、子 Agent 或网络工具）。使用私有 `pi-runtime` 配置，仅同步 `hub/...` 模型，不改动个人 pi 配置文件。
- **OpenCode V2**：独立进程，分析为 `plan`，修复为 `build`。
- **Antigravity**：`print` + 沙箱模式，分析为 `plan`，修复为 `accept-edits`。
- **Grok Build**：`prompt-file` 模式，分析为 `plan`，修复为 `acceptEdits`，禁用 Web 搜索与子 Agent。

`shuttle doctor` 仅检查可执行文件与 `gh` 权限可用性，不代表模型调用或账号额度已实测；各客户端保留各自登录态，无静默 fallback。

## 执行与发布边界

- 分析任务使用默认分支纯文本快照及 PR diff；修复任务使用固定 commit 的纯文本工作区，不启动项目插件。带 `.git` 的受控 checkout 和私密报告保存在 Agent 文件工具不可访问的相邻目录。
- `pi` 不运行仓库代码，因此自动修复不声称执行了测试；生成的草稿 PR 需要隔离 CI 或人工验证。
- 发布由控制器通过 `gh` 与 GitHub Git Data API 写入新 `shuttle/*` 分支，提交作者使用 GitHub noreply 地址，不运行仓库 git hooks、不执行本机 credential helper、不上传自由文本日志。发布前阻止隐藏控制文件、认证文件、路径逃逸、符号链接、过大或二进制变更及常见凭据格式；不会自动 merge、approve 或关闭 Issue。

## 存储与自启动

- **macOS**：默认 `~/Library/Application Support/Shuttle`，LaunchAgent 为 `local.shuttle.worker`（由 `shuttle install` 管理）。
- **Linux**：默认 `~/.local/share/Shuttle`，使用自定义进程管理器运行 `shuttle listen`。
- 可通过 `--data-dir` 覆盖目录；配置文件不保存 GitHub Token。

停止 macOS 后台服务（不删除本机报告或 GitHub 结果，重新运行 `shuttle install` 即可恢复）：

```bash
launchctl bootout "gui/$(id -u)/local.shuttle.worker"
```

## 目录结构

```text
shuttle/
├── Cargo.toml                  # Rust 包配置
├── src/
│   ├── main.rs                 # CLI 入口（init、policy、doctor、scan、once、listen、tasks、report、retry、publish、install）
│   ├── config.rs               # 本机配置、白名单、策略与 AgentRelay 迁移
│   ├── github.rs               # gh CLI 封装与 GitHub Git Data API 发布
│   ├── planner.rs              # 派发解析、权限校验与 SHA 绑定
│   ├── queue.rs                # 可恢复本地任务队列与 24 小时额度统计
│   ├── service.rs              # 轮询主循环、崩溃恢复与 LaunchAgent 安装
│   └── worker.rs               # Agent 适配执行、凭据扫描与工作区隔离
├── integrations/
│   └── pi-guard.mjs            # pi worker 专用受限 relay_file 工具守卫
└── tests/
    └── pi-guard.mjs            # pi-guard 路径与权限回归测试
```

## 验证

```bash
cargo test --locked
cargo fmt --check
cargo clippy --locked -- -D warnings
node tests/pi-guard.mjs
```

测试使用本机临时假 `gh` 离线运行，覆盖权限拒绝、精确命令解析、PR SHA 绑定、事件去重、崩溃恢复、Agent 参数与发布检查。

<details>
<summary>从 Agent Relay 升级</summary>

Shuttle 得名于织机中的梭子，负责在 GitHub 与本机 Agent 之间传递任务。

```bash
gh repo clone xdlkc/shuttle
cd shuttle
cargo build --release --locked
./target/release/shuttle install
shuttle doctor
shuttle tasks
```

已有 `AgentRelay/config.json` 时自动原位复用旧目录，保留策略、队列、报告与 Agent 引用。安装器将自启动切换到 `local.shuttle.worker` 并保留 `agent-relay` 兼容入口；旧评论标记与 `agent-relay/*` 分支仍被识别，避免重复派发。

</details>
