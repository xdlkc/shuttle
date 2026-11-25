# Shuttle

English | [简体中文](./README.zh-CN.md)

**Shuttle** is a standalone local-first Rust background service that connects GitHub Issues and Pull Requests to local coding agents through a recoverable task queue, posting structured status comments and draft PRs. It runs independently from Loom's inference gateway and quota tooling.

- **No public webhooks or uploaded keys**: Polls GitHub locally via `gh` every 60 seconds when idle, executing one task at a time without requiring a public callback URL, GitHub Actions runner, or uploading agent API keys to GitHub.
- **Explicit authorization & safety boundaries**: Manual dispatch via labels or slash commands requires repository `write`/`maintain`/`admin` permissions. PR tasks bind to the exact 40-character head SHA and cancel automatically if the body or head changes.
- **Controller-owned publication**: Agents never receive GitHub tokens. Validated text changes are committed by the controller via the GitHub Git Data API to a new `shuttle/*` branch as a draft PR, with secret scanning and path-escape checks before push.

## Quick Start

```bash
cargo build --release --locked
./target/release/shuttle init --repo owner/repo --agent pi
./target/release/shuttle policy --agent pi --model hub/deepseek/deepseek-flash --auto-fix true
./target/release/shuttle doctor
./target/release/shuttle bootstrap
./target/release/shuttle install
```

On a fresh machine, `init` handles maintainer-dispatched items only. Enable automatic fixing explicitly with `policy --auto-fix true`, or switch agents and models:

```bash
shuttle policy --agent pi --model hub/deepseek/deepseek-flash
shuttle policy --agent codex --model auto
shuttle policy --agent opencode2 --model hub/deepseek/deepseek-flash

# Switch to analyze-only automation
shuttle policy --auto-fix false --auto-triage true
```

When the machine sleeps or shuts down, processing pauses and resumes on wake by re-checking open Issues and PRs (using the [GitHub REST Issues API](https://docs.github.com/en/rest/issues/issues#list-repository-issues), which includes PRs). Draft PRs are never merged automatically.

## GitHub Dispatch

| Trigger | Behavior |
| --- | --- |
| `agent:codex` / `agent:antigravity` / `agent:pi` / `agent:opencode2` / `agent:grok` | Select an agent to analyze (requires exactly one routing label) |
| Issue + `agent:fix` | Approve the routed agent to fix the issue and open a draft PR |
| Comment `/agent pi analyze` | Dispatch a specific agent to analyze an Issue or PR |
| Issue comment `/agent codex fix` | Approve fixing the issue |
| PR comment `/agent codex fix <40-char head SHA>` | Approve fixing that exact PR head; creates a new draft PR without modifying the author's branch |
| `agent:paused` | Pause new tasks for this item and block publication of the current task |

Manual label/comment triggers accept only repository `write`/`maintain`/`admin` users. When `auto_fix` is enabled, the configured repository allowlist defines the fixed automatic policy for new items; issue bodies cannot override the agent, model, tools, or permissions. Draft PRs created by Shuttle do not auto-trigger themselves, though maintainers can dispatch follow-up tasks on them.

## Local Inspection & Recovery

```bash
shuttle tasks
shuttle report 1
shuttle retry 1
shuttle publish 1
shuttle policy --agent pi
shuttle policy --auto-fix false --auto-triage false
```

- `scan`: Discovers and records tasks without running agents or commenting on GitHub.
- `once` / `listen`: Runs a single polling cycle or loops continuously.
- `retry` / `publish`: `retry` re-runs the agent (consuming model quota again); `publish` retries GitHub publication for an existing patch without re-invoking the model. Publication network failures auto-retry up to 5 times (`publication_pending`); generation never auto-reruns after failure or restart.
- Task states: `queued`, `running`, `publishing`, `publication_pending`, `completed`, `failed`, `blocked`, `interrupted`, `cancelled`. Default timeout is 30 minutes.
- Rolling 24-hour quota cap for automatic analysis/fixes defaults to `20` (`auto_daily_limit`, configurable `1–100`); tasks exceeding the cap are marked `blocked`.

## Agent Adapters

- **Codex**: Uses [`codex exec`](https://developers.openai.com/cookbook/examples/codex/build_iterative_repair_loops_with_codex) (`read-only` for analysis, `workspace-write` for fixes), ignoring global execution overrides while keeping local auth.
- **pi**: Disables all built-in tools, MCP, skills, project configs, and extension discovery; exposes only the controller's `relay_file` tool (`list`/`read` for analysis, adding `write`/`remove` for fixes). No bash, subagents, or network tools. Uses a private `pi-runtime` config synced only for `hub/...` models without modifying personal pi config files.
- **OpenCode V2**: Separate process (`plan` for analysis, `build` for fixes).
- **Antigravity**: Print + sandbox mode (`plan` for analysis, `accept-edits` for fixes).
- **Grok Build**: Prompt-file mode (`plan` for analysis, `acceptEdits` for fixes), with web search and subagents disabled.

`shuttle doctor` verifies executable availability and `gh` permissions; it does not probe live model calls or account quotas. No silent fallback occurs between agents or models.

## Execution & Publication Boundaries

- Analysis runs on a plain-text snapshot of the default branch and PR diff; fixes run on a pinned-commit plain-text workspace without loading project plugins. Controlled `.git` checkouts and private reports live in sibling directories inaccessible to agent file tools.
- Because `pi` does not execute repository code, auto-fixes do not claim test execution; generated draft PRs require isolated CI or human review.
- Publication runs via `gh` and the GitHub Git Data API on a new `shuttle/*` branch using a GitHub noreply commit author—without executing repository git hooks or local credential helpers, and without uploading free-form agent logs. Pre-publish checks reject hidden control files, auth files, path escapes, symlinks, oversized/binary diffs, and common secret patterns.

## Storage & LaunchAgent

- **macOS**: `~/Library/Application Support/Shuttle` (LaunchAgent `local.shuttle.worker`, managed by `shuttle install`).
- **Linux**: `~/.local/share/Shuttle` (run `shuttle listen` under your own process supervisor).
- Override with `--data-dir`. Configuration files never store GitHub tokens.

Stop the macOS background service without deleting local reports or GitHub PRs:

```bash
launchctl bootout "gui/$(id -u)/local.shuttle.worker"
```

## Architecture

```text
shuttle/
├── Cargo.toml                  # Rust package manifest
├── src/
│   ├── main.rs                 # CLI entry point (init, policy, doctor, scan, once, listen, tasks, report, retry, publish, install)
│   ├── config.rs               # Local config, allowlists, policies & AgentRelay migration
│   ├── github.rs               # gh CLI wrapper & GitHub Git Data API publication
│   ├── planner.rs              # Dispatch parsing, authorization & SHA binding
│   ├── queue.rs                # Recoverable local task queue & daily quota tracking
│   ├── service.rs              # Polling loop, crash recovery & LaunchAgent installer
│   └── worker.rs               # Agent adapter execution, secret scanning & workspace isolation
├── integrations/
│   └── pi-guard.mjs            # Restricted relay_file tool guard for pi workers
└── tests/
    └── pi-guard.mjs            # Regression tests for pi-guard path & permission checks
```

## Verification

```bash
cargo test --locked
cargo fmt --check
cargo clippy --locked -- -D warnings
node tests/pi-guard.mjs
```

Tests run fully offline using a local fake `gh` binary, covering permission rejection, command parsing, PR SHA binding, event deduplication, crash recovery, agent arguments, and pre-publish secret checks.

<details>
<summary>Upgrading from Agent Relay</summary>

Shuttle takes its name from a loom shuttle carrying tasks between GitHub and local agents.

```bash
gh repo clone xdlkc/shuttle
cd shuttle
cargo build --release --locked
./target/release/shuttle install
shuttle doctor
shuttle tasks
```

If an existing `AgentRelay/config.json` is present, Shuttle reuses that directory in place—preserving policies, queues, reports, and agent references. The installer migrates LaunchAgent to `local.shuttle.worker` and keeps `agent-relay` as a compatibility wrapper. Legacy comment markers and `agent-relay/*` branches remain recognized to prevent duplicate dispatches.

</details>
