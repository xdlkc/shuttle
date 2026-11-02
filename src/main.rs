mod config;
mod github;
mod planner;
mod queue;
mod service;
mod worker;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Parser)]
#[command(version, about = "Shuttle — local GitHub Issue / PR dispatcher")]
struct Cli {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Create local configuration, without contacting agents or writing to GitHub.
    Init {
        #[arg(long)]
        repo: String,
        #[arg(long,default_value="pi",value_parser=config::AGENTS)]
        agent: String,
        #[arg(long, default_value_t = false)]
        auto_triage: bool,
    },
    /// Check CLI availability and GitHub access without generating a model request.
    Doctor,
    /// Add the routing labels to the allowlisted repositories.
    Bootstrap,
    /// Persist runtime policy; enabled=false pauses automatic analysis only.
    Policy {
        #[arg(long)]
        auto_triage: Option<bool>,
        #[arg(long)]
        auto_fix: Option<bool>,
        #[arg(long,value_parser=config::AGENTS)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Fetch changes and persist tasks; never launch an agent or post a comment.
    Scan,
    /// Fetch changes and execute one queued task.
    Once,
    /// Listen continuously, with one active task and durable crash recovery.
    Listen,
    Tasks,
    Report {
        id: i64,
    },
    Retry {
        id: i64,
    },
    /// Resume publication of an existing patch, without another model call.
    Publish {
        id: i64,
    },
    /// Install a separate user autostart service. Does not modify Loom.
    Install,
}
fn lock(data: &std::path::Path) -> Result<fs::File> {
    fs::create_dir_all(data)?;
    let mut opts = fs::OpenOptions::new();
    opts.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(data.join("worker.lock"))?;
    f.try_lock().map_err(|_| "another worker is running")?;
    Ok(f)
}
fn tick(
    c: &config::Config,
    gh: &github::GitHub,
    q: &queue::Queue,
    data: &std::path::Path,
) -> Result<()> {
    for link in q.links()? {
        if link.state == "merged" || !c.repositories.iter().any(|r| r.name == link.repo) {
            continue;
        }
        let pr = gh.api(
            "GET",
            &format!("repos/{}/pulls/{}", link.repo, link.number),
            None,
        )?;
        let state = if pr["merged"] == true {
            "merged"
        } else if pr["state"] == "closed" {
            "closed"
        } else {
            let reviews = gh.pages(&format!(
                "repos/{}/pulls/{}/reviews",
                link.repo, link.number
            ))?;
            let mut latest = std::collections::BTreeMap::new();
            for r in reviews {
                if r["commit_id"] == pr["head"]["sha"]
                    && ["APPROVED", "CHANGES_REQUESTED"]
                        .contains(&r["state"].as_str().unwrap_or(""))
                {
                    latest.insert(r["user"]["login"].to_string(), r["state"].clone());
                }
            }
            if latest.values().any(|v| v == "CHANGES_REQUESTED") {
                "changes_requested"
            } else {
                "awaiting_review"
            }
        };
        if state != link.state {
            q.link_state(link.task, state)?;
        }
    }
    planner::poll(c, gh, q)?;
    if let Some(t) = q
        .list()?
        .into_iter()
        .find(|t| t.state == "publication_pending")
    {
        if !planner::still_authorized(c, gh, &t)? {
            q.state(t.id, "cancelled", "authorization_changed")?;
            return Ok(());
        }
        q.state(t.id, "publishing", "")?;
        if let Err(error) = worker::resume_publication(c, gh, q, data, t.id) {
            let again = error.to_string().starts_with("github") && q.publication_retry(t.id)?;
            q.state(
                t.id,
                if again {
                    "publication_pending"
                } else {
                    "failed"
                },
                "publication_failed",
            )?;
            config::write(
                &data
                    .join("runs")
                    .join(t.id.to_string())
                    .join("control/failure.txt"),
                error.to_string().as_bytes(),
            )?;
        }
        return Ok(());
    }
    if let Some(t) = q.list()?.into_iter().find(|t| t.state == "queued") {
        if t.revision.starts_with("auto:") && q.recent_auto_runs()? >= c.auto_daily_limit {
            q.state(t.id, "blocked", "auto_daily_limit")?;
            return Ok(());
        }
        if let Err(error) = worker::run(c, gh, q, data, &t) {
            config::write(
                &data
                    .join("runs")
                    .join(t.id.to_string())
                    .join("control/failure.txt"),
                error.to_string().as_bytes(),
            )?;
            let publishing = q
                .list()?
                .iter()
                .any(|v| v.id == t.id && v.state == "publishing");
            let again = publishing
                && error.to_string().starts_with("github")
                && q.publication_retry(t.id)?;
            q.state(
                t.id,
                if again {
                    "publication_pending"
                } else {
                    "failed"
                },
                "execution_or_publication_failed",
            )?;
            let message = if again {
                "代码已生成，GitHub 发布稍后自动恢复；不会重新调用模型。"
            } else {
                "执行或发布未完成，任务已停止；不会自动重跑模型或切换 agent。"
            };
            let _ = gh.status(
                &t.repo,
                t.number,
                t.id,
                &format!(
                    "Shuttle · `{}` · 任务 #{}\n\n{} 本机检查：`shuttle report {}`。",
                    t.agent, t.id, message, t.id
                ),
            );
        }
    }
    Ok(())
}
fn run() -> Result<()> {
    let cli = Cli::parse();
    let data = cli.data_dir.unwrap_or_else(config::data_dir);
    if let Action::Init {
        repo,
        agent,
        auto_triage,
    } = &cli.command
    {
        if data.join("config.json").exists() {
            return Err("configuration already exists".into());
        }
        config::Config::initial(repo.clone(), agent.clone(), *auto_triage)?.save(&data)?;
        println!("本机配置已创建。运行 shuttle doctor / bootstrap / install。");
        return Ok(());
    }
    let mut c = config::Config::load(&data)?;
    match cli.command {
        Action::Init { .. } => unreachable!(),
        Action::Policy {
            auto_triage,
            auto_fix,
            agent,
            model,
        } => {
            if let Some(enabled) = auto_triage {
                for r in &mut c.repositories {
                    r.auto_triage = enabled
                }
            }
            if let Some(enabled) = auto_fix {
                for r in &mut c.repositories {
                    r.auto_fix = enabled
                }
            }
            if let Some(agent) = agent {
                if agent != c.default_agent && model.is_none() {
                    c.default_model = None;
                }
                c.default_agent = agent
            }
            if let Some(model) = model {
                c.default_model = if model == "auto" { None } else { Some(model) };
            }
            if c.default_model.as_ref().is_some_and(|s| {
                s.is_empty()
                    || s.len() > 200
                    || s.chars().any(char::is_whitespace)
                    || s.starts_with('-')
            }) {
                return Err("invalid model".into());
            }
            c.save(&data)?;
            println!("本机策略已保存，下轮检查生效。");
        }
        Action::Doctor => {
            let gh = github::GitHub::new()?;
            for r in &c.repositories {
                let v = gh.api("GET", &format!("repos/{}", r.name), None)?;
                println!(
                    "{} · issues={} · can_push={}",
                    r.name, v["has_issues"], v["permissions"]["push"]
                );
            }
            for agent in config::AGENTS {
                println!(
                    "{agent}: {}",
                    if c.agents.get(agent).is_some_and(|a| a.executable.is_file()) {
                        "executable found (login/runtime not yet verified)"
                    } else {
                        "unavailable"
                    }
                );
            }
            println!(
                "默认 agent：{}；轮询：{} 秒；并发：1；自动合并：关闭",
                c.default_agent, c.poll_seconds
            );
            println!(
                "默认模型：{}；自动调用上限：{}/24h",
                c.default_model.as_deref().unwrap_or("agent 自身默认"),
                c.auto_daily_limit
            );
        }
        Action::Bootstrap => {
            let gh = github::GitHub::new()?;
            for r in &c.repositories {
                let existing = gh.pages(&format!("repos/{}/labels", r.name))?;
                for label in config::AGENTS
                    .map(|a| format!("agent:{a}"))
                    .into_iter()
                    .chain(["agent:fix".into(), "agent:paused".into()])
                {
                    if !existing.iter().any(|v| v["name"] == label) {
                        gh.api("POST",&format!("repos/{}/labels",r.name),Some(json!({"name":label,"color":"25634D","description":"Shuttle routing; only repository writers authorize execution"})))?;
                    }
                }
                println!("{} 标签已就绪。", r.name);
            }
        }
        Action::Scan => {
            let gh = github::GitHub::new()?;
            let q = queue::Queue::open(&data)?;
            println!("新增排队任务：{}（未执行）", planner::poll(&c, &gh, &q)?);
        }
        Action::Tasks => {
            let q = queue::Queue::open(&data)?;
            for t in q.list()? {
                println!(
                    "{}\t{}#{}\t{}\t{}\t{}",
                    t.id, t.repo, t.number, t.agent, t.mode, t.state
                )
            }
            for link in q.links()? {
                println!("PR\t{}\t{}\t{}", link.task, link.state, link.url);
            }
            if let Ok(bytes) = fs::read(data.join("heartbeat.json")) {
                println!("heartbeat: {}", String::from_utf8_lossy(&bytes));
            }
        }
        Action::Report { id } => {
            let t = queue::Queue::open(&data)?
                .list()?
                .into_iter()
                .find(|t| t.id == id)
                .ok_or("task not found")?;
            println!(
                "{}#{} · {} · {} · {}",
                t.repo, t.number, t.agent, t.mode, t.state
            );
            let p = data
                .join("runs")
                .join(id.to_string())
                .join("control/report.md");
            if let Ok(text) = fs::read_to_string(p) {
                println!("{text}")
            } else {
                println!("报告尚不可用。原始日志只保存在本机 runs/<任务号>/control/。");
            }
            if let Ok(text) = fs::read_to_string(
                data.join("runs")
                    .join(id.to_string())
                    .join("control/failure.txt"),
            ) {
                println!("本机失败原因：{text}");
            }
        }
        Action::Retry { id } => {
            queue::Queue::open(&data)?.retry(id)?;
            println!("任务已重新排队；执行前会重新验证授权。")
        }
        Action::Publish { id } => {
            let _lock = lock(&data)?;
            let gh = github::GitHub::new()?;
            let q = queue::Queue::open(&data)?;
            if let Err(error) = worker::resume_publication(&c, &gh, &q, &data, id) {
                config::write(
                    &data
                        .join("runs")
                        .join(id.to_string())
                        .join("control/failure.txt"),
                    error.to_string().as_bytes(),
                )?;
                return Err(error);
            }
            println!("已有补丁发布完成，未重新调用模型。");
        }
        Action::Once => {
            let _lock = lock(&data)?;
            let gh = github::GitHub::new()?;
            let q = queue::Queue::open(&data)?;
            q.recover()?;
            tick(&c, &gh, &q, &data)?;
        }
        Action::Listen => {
            let _lock = lock(&data)?;
            let gh = github::GitHub::new()?;
            let q = queue::Queue::open(&data)?;
            q.recover()?;
            loop {
                let ok = if let Ok(next) = config::Config::load(&data) {
                    c = next;
                    tick(&c, &gh, &q, &data).is_ok()
                } else {
                    false
                };
                let heartbeat = json!({"checked_at":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"github_poll_ok":ok});
                config::write(
                    &data.join("heartbeat.json"),
                    &serde_json::to_vec(&heartbeat)?,
                )?;
                std::thread::sleep(Duration::from_secs(c.poll_seconds));
            }
        }
        Action::Install => service::install(&data)?,
    }
    Ok(())
}
fn main() {
    if run().is_err() {
        eprintln!(
            "Shuttle 操作未完成。请检查本机配置、GitHub 权限与任务报告；未输出底层错误或凭据。"
        );
        std::process::exit(1)
    }
}
