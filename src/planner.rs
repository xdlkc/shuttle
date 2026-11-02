use crate::{
    Result,
    config::{self, Config},
    github::GitHub,
    queue::{Queue, Task},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub fn digest(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn command(body: &str) -> Option<(String, String, Option<String>)> {
    let fields: Vec<_> = body.lines().next()?.split_whitespace().collect();
    if !(3..=4).contains(&fields.len())
        || fields[0] != "/agent"
        || !config::AGENTS.contains(&fields[1])
        || !["analyze", "fix"].contains(&fields[2])
    {
        return None;
    }
    if fields.len() == 4 && !config::sha_valid(fields[3]) {
        return None;
    }
    Some((
        fields[1].into(),
        fields[2].into(),
        fields.get(3).map(|s| s.to_string()),
    ))
}
pub fn active_labels(events: &[Value]) -> BTreeMap<String, Value> {
    let mut labels = BTreeMap::new();
    for e in events {
        let Some(label) = e["label"]["name"]
            .as_str()
            .filter(|s| s.starts_with("agent:"))
        else {
            continue;
        };
        match e["event"].as_str() {
            Some("labeled") => {
                labels.insert(label.into(), e.clone());
            }
            Some("unlabeled") => {
                labels.remove(label);
            }
            _ => {}
        }
    }
    labels
}
fn generated(v: &Value) -> bool {
    v["body"].as_str().is_some_and(|b| {
        ["<!-- shuttle:output -->", "<!-- agent-relay:output -->"]
            .iter()
            .any(|marker| b.contains(marker))
    }) || v["user"]["type"] == "Bot"
}
pub fn poll(c: &Config, gh: &GitHub, q: &Queue) -> Result<usize> {
    let mut added = 0;
    for repo in &c.repositories {
        let issues = gh.pages(&format!("repos/{}/issues?state=open", repo.name))?;
        if issues.is_empty() {
            continue;
        }
        let head = gh.api("GET", &format!("repos/{}/commits/HEAD", repo.name), None)?;
        let base_sha = head["sha"]
            .as_str()
            .filter(|s| config::sha_valid(s))
            .ok_or("head unavailable")?;
        for issue in issues {
            if issue["labels"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v["name"] == "agent:paused"))
            {
                continue;
            }
            let number = issue["number"].as_u64().ok_or("issue number missing")?;
            let is_pr = issue.get("pull_request").is_some();
            let pr = if is_pr {
                gh.api("GET", &format!("repos/{}/pulls/{number}", repo.name), None)?
            } else {
                Value::Null
            };
            let sha = if is_pr {
                pr["head"]["sha"]
                    .as_str()
                    .filter(|s| config::sha_valid(s))
                    .ok_or("pr head missing")?
            } else {
                base_sha
            };
            let fingerprint = digest(&format!(
                "{}\n{}\n{}",
                issue["title"].as_str().unwrap_or(""),
                issue["body"].as_str().unwrap_or(""),
                if is_pr { sha } else { "" }
            ));
            let events = gh.pages(&format!("repos/{}/issues/{number}/events", repo.name))?;
            let comments = gh.pages(&format!("repos/{}/issues/{number}/comments", repo.name))?;
            let request = json!({"title":issue["title"],"body":issue["body"],"comments":comments.iter().rev().filter(|v|!v["body"].as_str().is_some_and(|s|["<!-- shuttle:", "<!-- agent-relay:"].iter().any(|marker| s.contains(marker)))).take(12).map(|v|json!({"author":v["user"]["login"],"body":v["body"]})).collect::<Vec<_>>(),"base_sha":base_sha});
            let mut candidates = vec![];
            let labels = active_labels(&events);
            let selected: Vec<_> = config::AGENTS
                .into_iter()
                .filter(|a| labels.contains_key(&format!("agent:{a}")))
                .collect();
            if selected.len() == 1 {
                let agent = selected[0];
                let event = &labels[&format!("agent:{agent}")];
                let actor = event["actor"]["login"].as_str().unwrap_or("");
                let fixing = labels.get("agent:fix");
                if gh.writer(&repo.name, actor)
                    && fixing.is_none_or(|e| {
                        gh.writer(&repo.name, e["actor"]["login"].as_str().unwrap_or(""))
                    })
                {
                    // A label cannot bind a PR approval to an exact commit. Use a SHA-pinned command for PR fixes.
                    if fixing.is_none() || !is_pr {
                        let mode = if fixing.is_some() { "fix" } else { "analyze" };
                        let revision = format!(
                            "label:{}:{}",
                            event["id"],
                            fixing.map(|v| v["id"].to_string()).unwrap_or_default()
                        );
                        candidates.push((agent.to_string(),mode.to_string(),revision,json!({"kind":"label","actor":actor,"agent":agent,"event_id":event["id"],"fix_id":fixing.map(|e|e["id"].clone()),"fix_actor":fixing.map(|e|e["actor"]["login"].clone())})));
                    }
                }
            }
            for comment in &comments {
                let Some((agent, mode, pin)) = command(comment["body"].as_str().unwrap_or(""))
                else {
                    continue;
                };
                let actor = comment["user"]["login"].as_str().unwrap_or("");
                if !gh.writer(&repo.name, actor)
                    || (is_pr && mode == "fix" && pin.as_deref() != Some(sha))
                {
                    continue;
                }
                let revision = format!("comment:{}:{}", comment["id"], comment["updated_at"]);
                candidates.push((agent.clone(),mode.clone(),revision,json!({"kind":"comment","actor":actor,"id":comment["id"],"body":comment["body"]})));
            }
            if candidates.is_empty()
                && selected.is_empty()
                && (repo.auto_triage || repo.auto_fix)
                && !generated(&issue)
            {
                candidates.push((
                    c.default_agent.clone(),
                    if repo.auto_fix { "fix" } else { "analyze" }.into(),
                    format!("auto:{fingerprint}"),
                    json!({"kind":"auto"}),
                ));
            }
            for (agent, mode, revision, proof) in candidates {
                let mut r = request.clone();
                r["proof"] = proof;
                r["model"] = if agent == c.default_agent {
                    json!(c.default_model)
                } else {
                    Value::Null
                };
                if q.insert(&Task {
                    id: 0,
                    repo: repo.name.clone(),
                    number,
                    agent,
                    mode,
                    revision,
                    state: "queued".into(),
                    request: serde_json::to_string(&r)?,
                    sha: sha.into(),
                    is_pr,
                })? {
                    added += 1
                }
            }
        }
    }
    Ok(added)
}
pub fn still_authorized(c: &Config, gh: &GitHub, t: &Task) -> Result<bool> {
    let Some(repo) = c.repositories.iter().find(|r| r.name == t.repo) else {
        return Ok(false);
    };
    let issue = gh.api(
        "GET",
        &format!("repos/{}/issues/{}", t.repo, t.number),
        None,
    )?;
    if issue["state"] != "open"
        || issue["labels"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v["name"] == "agent:paused"))
    {
        return Ok(false);
    }
    if t.is_pr {
        let pr = gh.api("GET", &format!("repos/{}/pulls/{}", t.repo, t.number), None)?;
        if pr["head"]["sha"] != t.sha {
            return Ok(false);
        }
    }
    let r: Value = serde_json::from_str(&t.request)?;
    let p = &r["proof"];
    if issue["title"] != r["title"] || issue["body"] != r["body"] {
        return Ok(false);
    }
    match p["kind"].as_str() {
        Some("auto") => Ok((if t.mode == "fix" {
            repo.auto_fix
        } else {
            repo.auto_triage && !repo.auto_fix
        }) && !generated(&issue)),
        Some("comment") => {
            let id = p["id"].as_u64().ok_or("comment id missing")?;
            let v = gh.api(
                "GET",
                &format!("repos/{}/issues/comments/{id}", t.repo),
                None,
            )?;
            Ok(v["body"] == p["body"]
                && v["user"]["login"] == p["actor"]
                && gh.writer(&t.repo, p["actor"].as_str().unwrap_or("")))
        }
        Some("label") => {
            let labels =
                active_labels(&gh.pages(&format!("repos/{}/issues/{}/events", t.repo, t.number))?);
            let agents = labels
                .keys()
                .filter(|k| config::AGENTS.iter().any(|a| *k == &format!("agent:{a}")))
                .count();
            Ok(agents == 1
                && labels
                    .get(&format!("agent:{}", t.agent))
                    .is_some_and(|e| e["actor"]["login"] == p["actor"] && e["id"] == p["event_id"])
                && gh.writer(&t.repo, p["actor"].as_str().unwrap_or(""))
                && if t.mode == "fix" {
                    labels.get("agent:fix").is_some_and(|e| {
                        Some(&e["actor"]["login"]) == p.get("fix_actor")
                            && Some(&e["id"]) == p.get("fix_id")
                            && gh.writer(&t.repo, e["actor"]["login"].as_str().unwrap_or(""))
                    })
                } else {
                    !labels.contains_key("agent:fix")
                })
        }
        _ => Ok(false),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_is_exact_data_not_shell() {
        assert!(command("/agent pi analyze").is_some());
        assert!(command(&format!("/agent codex fix {}", "a".repeat(40))).is_some());
        for s in [
            "/agent codex fix $(id)",
            "/agent evil fix",
            "text /agent pi fix",
            "/agent pi fix HEAD",
            "/agent pi analyze --dangerous",
            "/agent pi",
        ] {
            assert!(command(s).is_none(), "{s}")
        }
    }
    #[test]
    fn removed_labels_do_not_authorize() {
        let events = vec![
            json!({"event":"labeled","label":{"name":"agent:pi"}}),
            json!({"event":"unlabeled","label":{"name":"agent:pi"}}),
            json!({"event":"labeled","label":{"name":"agent:codex"}}),
        ];
        let active = active_labels(&events);
        assert!(!active.contains_key("agent:pi"));
        assert!(active.contains_key("agent:codex"));
    }
    #[cfg(unix)]
    struct Fixture {
        root: tempfile::TempDir,
        gh: GitHub,
        config: Config,
        queue: Queue,
    }
    #[cfg(unix)]
    impl Fixture {
        fn new(auto: bool, is_pr: bool) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let p = root.path();
            let script = format!(
                "#!/bin/sh\ncase \"$6\" in\n */issues\\?*) cat '{}/issues';;\n */events\\?*) cat '{}/events';;\n */comments\\?*) cat '{}/comments';;\n */issues/1) cat '{}/issue';;\n */pulls/1) cat '{}/pr';;\n */commits/HEAD) printf '%s' '{{\"sha\":\"{}\"}}';;\n */collaborators/writer/permission) printf '%s' '{{\"permission\":\"write\"}}';;\n */collaborators/reader/permission) printf '%s' '{{\"permission\":\"read\"}}';;\n *) exit 1;;\nesac\n",
                p.display(),
                p.display(),
                p.display(),
                p.display(),
                p.display(),
                "a".repeat(40)
            );
            let ghfile = p.join("gh");
            std::fs::write(&ghfile, script).unwrap();
            std::fs::set_permissions(&ghfile, std::fs::Permissions::from_mode(0o700)).unwrap();
            let c = Config {
                repositories: vec![crate::config::Repository {
                    name: "owner/repo".into(),
                    auto_triage: auto,
                    auto_fix: false,
                }],
                default_agent: "codex".into(),
                default_model: None,
                poll_seconds: 60,
                timeout_seconds: 60,
                auto_daily_limit: 20,
                agents: Default::default(),
            };
            let q = Queue::open(&p.join("data")).unwrap();
            let f = Self {
                root,
                gh: GitHub::fixture(ghfile),
                config: c,
                queue: q,
            };
            let mut issue = json!({"number":1,"title":"fixture","body":"untrusted fixture","state":"open","user":{"type":"User"},"labels":[]});
            if is_pr {
                issue["pull_request"] = json!({"url":"fixture"});
            }
            f.set("issue", issue.clone());
            f.set("issues", json!([issue]));
            f.set("events", json!([]));
            f.set("comments", json!([]));
            f.set("pr", json!({"head":{"sha":"b".repeat(40)}}));
            f
        }
        fn set(&self, name: &str, v: Value) {
            std::fs::write(self.root.path().join(name), serde_json::to_vec(&v).unwrap()).unwrap()
        }
        fn poll(&self) -> usize {
            poll(&self.config, &self.gh, &self.queue).unwrap()
        }
        fn label(&self, actor: &str) {
            self.set("events",json!([{"event":"labeled","id":10,"actor":{"login":actor},"label":{"name":"agent:codex"}}]));
        }
    }
    #[cfg(unix)]
    #[test]
    fn reader_cannot_dispatch_but_writer_can() {
        let f = Fixture::new(false, false);
        f.label("reader");
        assert_eq!(f.poll(), 0);
        f.label("writer");
        assert_eq!(f.poll(), 1);
        assert_eq!(f.poll(), 0);
        let t = &f.queue.list().unwrap()[0];
        assert!(still_authorized(&f.config, &f.gh, t).unwrap());
        f.set("events", json!([]));
        assert!(!still_authorized(&f.config, &f.gh, t).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn changing_issue_text_does_not_reuse_an_old_approval() {
        let f = Fixture::new(false, false);
        f.label("writer");
        assert_eq!(f.poll(), 1);
        let t = f.queue.list().unwrap().remove(0);
        let mut issue: Value =
            serde_json::from_slice(&std::fs::read(f.root.path().join("issue")).unwrap()).unwrap();
        issue["body"] = json!("changed by an external submitter");
        f.set("issue", issue.clone());
        f.set("issues", json!([issue]));
        assert!(!still_authorized(&f.config, &f.gh, &t).unwrap());
        assert_eq!(f.poll(), 0);
    }
    #[cfg(unix)]
    #[test]
    fn pr_fix_requires_writer_and_current_full_sha() {
        let f = Fixture::new(false, true);
        for (actor, body, expected) in [
            ("writer", "/agent codex fix".to_string(), 0),
            ("reader", format!("/agent codex fix {}", "b".repeat(40)), 0),
            ("writer", format!("/agent codex fix {}", "a".repeat(40)), 0),
            ("writer", format!("/agent codex fix {}", "b".repeat(40)), 1),
        ] {
            f.set(
                "comments",
                json!([{"id":7,"updated_at":"fixture","user":{"login":actor},"body":body}]),
            );
            assert_eq!(f.poll(), expected);
        }
        let t = f.queue.list().unwrap().remove(0);
        assert!(still_authorized(&f.config, &f.gh, &t).is_err()); // Mock intentionally has no comment-by-id endpoint: fail closed.
        f.set("pr", json!({"head":{"sha":"c".repeat(40)}}));
        assert!(!still_authorized(&f.config, &f.gh, &t).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn own_status_comments_do_not_loop_or_rebill() {
        let f = Fixture::new(true, false);
        assert_eq!(f.poll(), 1);
        let t = &f.queue.list().unwrap()[0];
        f.queue.state(t.id, "completed", "").unwrap();
        f.set("comments", json!([{"body":"<!-- agent-relay:1 --> done"}]));
        assert_eq!(f.poll(), 0);
        let mut issue: Value =
            serde_json::from_slice(&std::fs::read(f.root.path().join("issue")).unwrap()).unwrap();
        issue["body"] = json!("<!-- agent-relay:output --> related to a draft PR");
        f.set("issues", json!([issue]));
        assert_eq!(f.poll(), 0);
    }
    #[test]
    fn renamed_and_legacy_output_markers_both_prevent_self_dispatch() {
        for marker in ["<!-- shuttle:output -->", "<!-- agent-relay:output -->"] {
            assert!(generated(&json!({"body":marker,"user":{"type":"User"}})));
        }
        assert!(!generated(
            &json!({"body":"ordinary issue","user":{"type":"User"}})
        ));
    }
    #[cfg(unix)]
    #[test]
    fn owner_can_enable_automatic_fixes_with_pinned_model() {
        let mut f = Fixture::new(false, true);
        f.config.repositories[0].auto_fix = true;
        f.config.default_agent = "pi".into();
        f.config.default_model = Some("hub/deepseek/deepseek-flash".into());
        assert_eq!(f.poll(), 1);
        let t = f.queue.list().unwrap().remove(0);
        assert_eq!(t.agent, "pi");
        assert_eq!(t.mode, "fix");
        assert_eq!(t.sha, "b".repeat(40));
        let r: Value = serde_json::from_str(&t.request).unwrap();
        assert_eq!(r["model"], "hub/deepseek/deepseek-flash");
        assert!(still_authorized(&f.config, &f.gh, &t).unwrap());
        f.config.repositories[0].auto_fix = false;
        assert!(!still_authorized(&f.config, &f.gh, &t).unwrap());
    }
}
