# Shuttle

独立的 Rust 本机后台服务：GitHub Issue / PR → 可恢复任务队列 → 编程 agent → 状态评论 / 草稿 PR。与 Loom 的推理进程和额度工具分别运行。

## 使用

```sh
cargo build --release --locked
./target/release/shuttle init --repo owner/repo --agent pi
./target/release/shuttle policy --agent pi --model hub/deepseek/deepseek-flash --auto-fix true
./target/release/shuttle doctor
./target/release/shuttle bootstrap
./target/release/shuttle install
```

可选择自动处理新 Issue / PR，默认使用 pi + `hub/deepseek/deepseek-flash`，也可切换 agent 或模型。修复结果为草稿 PR，合并由维护者决定。自动任务参数不改变个人 pi 的全局默认配置。

`init` 在新机器默认只处理维护者派发；通过上面的 `policy --auto-fix true` 明确启用自动修复。切换默认 agent / 模型：

```sh
shuttle policy --agent pi --model hub/deepseek/deepseek-flash
shuttle policy --agent codex --model auto
shuttle policy --agent opencode2 --model hub/deepseek/deepseek-flash
```

改为只自动分析：

```sh
shuttle policy --auto-fix false --auto-triage true
```

服务空闲时每 60 秒通过本机 `gh` 查询 GitHub，单任务执行，任务结束后进入下一轮。无需公网回调地址、GitHub Actions runner 或将 agent Key 上传到 GitHub。电脑关机或休眠时暂停处理，恢复后重新检查当前所有打开的 Issue / PR。GitHub API 的 Issues 列表也包含 PR；基于[官方接口](https://docs.github.com/en/rest/issues/issues#list-repository-issues)实现。

## 在 GitHub 派发

| 操作 | 行为 |
|---|---|
| `agent:codex` / `agent:antigravity` / `agent:pi` / `agent:opencode2` / `agent:grok` | 选择一个 agent 分析，需恰好一个路由标签 |
| Issue 另加 `agent:fix` | 批准该 agent 修复并产生草稿 PR |
| 评论 `/agent pi analyze` | 指定 agent 分析 Issue / PR |
| Issue 评论 `/agent codex fix` | 批准修复 Issue |
| PR 评论 `/agent codex fix <完整的 40 位 head SHA>` | 批准修复这个精确版本；生成新的草稿 PR，不改原作者分支 |
| `agent:paused` | 暂停该条目的新任务，并阻止当前任务发布 |

标签和评论的手动派发只接受仓库 write / maintain / admin 用户；作者身份或标签文字本身不构成授权。启用 `auto_fix` 后，仓库允许列表本身构成对新条目的自动处理策略，不要求报告者拥有写权限；仅按固定策略执行，正文不能改 agent、模型、工具或权限。PR 自动任务绑定发现时的完整 head SHA，生成前后均重新核对。正文或 head 在排队后改变，旧任务取消，下一轮自动发现新版本。维护者手动派发则需重新发表评论或重新添加标签。服务自身创建的 PR 不自动触发自己，仍接受维护者明确派发；状态评论不触发重复计费。

## 本机查看与恢复

```sh
shuttle tasks
shuttle report 1
shuttle retry 1
shuttle publish 1
shuttle policy --agent pi
shuttle policy --auto-fix false --auto-triage false
```

`scan` 只发现并保存任务，不执行 agent，也不向 GitHub 写评论。`once` 执行一轮；`listen` 持续运行。失败或进程重启后的任务不会自动重新生成，不自动切换 agent。`retry` 是重新执行 agent，可能再次消耗模型用量；`publish` 仅恢复已有补丁的 GitHub 发布，不再次调用模型。后台对发布阶段的网络失败最多自动恢复 5 次；恢复期间保持 publication_pending。生成阶段不会自动重跑。

状态包括 queued / running / publishing / publication_pending / completed / failed / blocked / interrupted / cancelled。限时默认 30 分钟；状态、控制台日志与报告均保存在私有本机目录。自动化默认不上传 agent 的自由文本回复、原始日志和源会话，只回写固定格式状态与经过内容检查的草稿 PR。分析结论用 `shuttle report <任务号>` 在本机查看。

创建草稿 PR 后继续跟踪 awaiting_review / changes_requested / merged / closed。审查人请求修改后，可在该 PR 上用同样的 `/agent <名称> fix <完整 head SHA>` 分派后续任务；首版生成新的草稿 PR，不覆盖原分支。生成的 PR 默认不会自动分析自己，但接受维护者的明确派发。

自动分析 / 修复默认每滚动 24 小时最多运行 20 次，达到上限的任务标为 blocked；不会自动换模型、换订阅或重试。维护者明确派发的任务单独处理。可在本机配置中修改 `auto_daily_limit`（1–100）。

## Agent 适配

- Codex：使用官方支持的 [`codex exec`](https://developers.openai.com/cookbook/examples/codex/build_iterative_repair_loops_with_codex)，分析为 read-only，修复为 workspace-write，忽略用户全局执行配置并保留本机认证。
- pi：禁用全部内置工具、MCP、skills、项目配置与扩展发现，仅加载控制器提供的 `relay_file` 文件工具。允许在源码工作区内 list / read，修复增加 write / remove；没有 bash、子 agent、网络工具或任意代码执行工具。私密路径、隐藏控制文件、符号链接及工作区逃逸均拒绝。扩展加载失败时也不会回落到内置工具。
- OpenCode V2：独立进程，分析为 plan，修复为 build。
- Antigravity：print + sandbox，分析为 plan，修复为 accept-edits；本机已存在的全放行包装脚本不会作为默认执行入口。
- Grok Build：prompt-file，分析为 plan，修复为 acceptEdits；禁用 Web 搜索与子 agent。

`doctor` 仅表明执行文件和 GitHub 权限可用，不代表全部 agent 的账号、参数或模型调用都已实测。各客户端保留各自登录状态、订阅和模型选择；无静默 fallback。服务不会复制 OAuth、Key 或浏览器登录态到 GitHub。

pi worker 使用本机私有的 `pi-runtime` 配置，只从个人 pi 的模型目录同步 `hub` 条目及默认思考档位，凭据仍通过原有本机 Key helper 读取。模型参数仅作用于这个 worker；个人 pi 的 models / settings / auth 文件保持原样。当前 pi worker 接受 `hub/...` 模型，不导入个人的其他 OAuth provider；改用其他原生账号时应选择对应 agent 或另建显式配置。

## 执行与发布边界

分析任务使用默认分支的纯文本快照及 PR diff；修复任务使用固定 commit 的纯文本工作区，不启动项目插件。带 `.git` 的受控 checkout 和私密报告保存在 agent 文件工具不可访问的相邻目录；控制器将验证后的文本改动应用回 checkout。pi 不运行仓库代码，因此自动修复不声称执行了测试；生成的草稿 PR 需要隔离 CI 或人工验证。个人开发目录保留。Codex 使用其原生权限沙箱；其他 CLI 的 plan / 工具白名单不是容器隔离，切换 agent 时应按该客户端的实际权限能力决定是否用于不可信自动任务。

发布由控制器执行，agent 不取得 GitHub Token 环境变量。代码变更通过 GitHub Git Data API 写入新的 `shuttle/*` 分支并创建 draft PR，不运行仓库 git hooks、不执行本机 git credential helper、不上传自由文本报告。提交作者使用 GitHub noreply 地址。发布前阻止隐藏控制文件、认证文件、路径逃逸、符号链接、过大或二进制变更、已知本机 Key / OAuth / GitHub Token 和常见凭据格式。此扫描是补充检查，不等同于对任意未知私密信息的数学保证。不会自动 merge、approve、关闭用户 Issue 或改原 PR 分支。

控制器通过 `gh` 完成 GitHub 操作，本机 GitHub 账号在仓库评论中可见。大规模、多人或公开服务应迁移到专用 GitHub App 身份和隔离 worker。无需将现有个人账号的广域 Token 放进 agent prompt。

## 存储和自启动

macOS 默认 `~/Library/Application Support/Shuttle`，LaunchAgent 为 `local.shuttle.worker`；Linux 默认 `~/.local/share/Shuttle`，目前用自己的进程管理器运行 `shuttle listen`。`--data-dir` 可覆盖目录。配置不保存 Token。安装命令目前仅自动管理 macOS 自启动。

```sh
launchctl bootout "gui/$(id -u)/local.shuttle.worker"
```

这会停止后台服务；不删除本机报告或 GitHub 上的结果。恢复时重新运行 `shuttle install`。

## 验证

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked -- -D warnings
node tests/pi-guard.mjs
```

测试使用本机临时假 `gh`，覆盖权限拒绝、精确命令、PR SHA 绑定、授权失效、事件去重、崩溃恢复、agent 参数和发布检查。真实端到端结果记录在本机验收报告，不将真实使用记录提交到源代码。

## 从 Agent Relay 升级

Shuttle 的名称来自织机中的梭子，负责在 GitHub 和本机 agent 之间传递任务；与 Loom 网关、Spool 额度工具分别安装。

```sh
gh repo clone xdlkc/shuttle
cd shuttle
cargo build --release --locked
./target/release/shuttle install
shuttle doctor
shuttle tasks
```

已有 `AgentRelay/config.json` 时自动原位复用旧目录，保留策略、队列、报告和 agent 登录引用，不重新初始化。安装器将自启动切换到 `local.shuttle.worker`，停用旧任务后才启动新任务；原来由安装器创建的 `agent-relay` 命令作为兼容入口保留。新机器使用 `Shuttle` 数据目录。旧评论标记和已创建的 `agent-relay/*` 分支仍被识别，避免重复派发或重复创建草稿 PR。

仓库只包含源码、文档和合成测试；不包含本机配置、任务数据库、报告、运行日志、agent 认证或个人使用记录。
