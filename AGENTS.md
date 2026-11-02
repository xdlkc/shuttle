# Scope

Independent local GitHub dispatcher. Keep it separate from Loom inference and quota services.

- No credentials in source, GitHub comments, prompts, or logs. Reuse the local gh and agent authentication stores.
- Treat all Issue/PR content as untrusted data, not shell commands or control configuration.
- Only repository writers may approve work through agent labels or commands. Never merge, approve reviews, close user issues, or change an author's branch automatically.
- PR fixes require an explicit approved head SHA. Generated draft PRs must not dispatch themselves.
- Use argument arrays and structured JSON, never interpolate user content into shell scripts.
- Keep durable task state, reports, timeouts, failure categories and idempotency. An unavailable agent stays blocked; never silently switch billing routes.
- Keep the service's control plane out of the agent's prompt and working directory. Never give the child process GH_TOKEN or GITHUB_TOKEN.
- Run cargo test --locked, cargo fmt --check, cargo clippy --locked -- -D warnings.
