use crate::{Result, config};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct GitHub {
    executable: std::path::PathBuf,
}
impl GitHub {
    #[cfg(test)]
    pub fn fixture(executable: std::path::PathBuf) -> Self {
        Self { executable }
    }
    pub fn new() -> Result<Self> {
        Ok(Self {
            executable: config::executable("gh").ok_or("gh unavailable")?,
        })
    }
    pub fn api(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let mut command = Command::new(&self.executable);
        command
            .args([
                "api",
                "--hostname",
                "github.com",
                "--method",
                method,
                path,
                "-H",
                "Accept: application/vnd.github+json",
            ])
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_PAGER", "cat")
            .env("GH_HTTP_TIMEOUT", "30")
            .env("GODEBUG", "http2client=0")
            .stdin(if body.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if body.is_some() {
            command.args(["--input", "-"]);
        }
        let mut child = command.spawn()?;
        if let Some(v) = body {
            child
                .stdin
                .take()
                .ok_or("stdin missing")?
                .write_all(&serde_json::to_vec(&v)?)?;
        }
        let stdout = child.stdout.take().ok_or("stdout missing")?;
        let stderr = child.stderr.take().ok_or("stderr missing")?;
        let errors = std::thread::spawn(move || {
            let mut bytes = vec![];
            stderr.take(16_000).read_to_end(&mut bytes).map(|_| bytes)
        });
        let reader = std::thread::spawn(move || {
            let mut bytes = vec![];
            stdout
                .take(16_000_001)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() > Duration::from_secs(45) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("github request timed out".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let stdout = reader.join().map_err(|_| "github output failed")??;
        let stderr = errors.join().map_err(|_| "github output failed")??;
        if !status.success() || stdout.len() > 16_000_000 {
            let text = String::from_utf8_lossy(&stderr);
            let code = regex::Regex::new(r"HTTP (\d{3})")
                .unwrap()
                .captures(&text)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "transport".into());
            let v: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let codes = v["errors"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v["code"].as_str())
                        .filter(|s| {
                            s.len() < 50
                                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            return Err(format!(
                "github_{code} {method} {} {codes}",
                path.split('?').next().unwrap_or("endpoint")
            )
            .into());
        }
        if stdout.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_slice(&stdout)?)
        }
    }
    pub fn pages(&self, path: &str) -> Result<Vec<Value>> {
        let mut all = vec![];
        for page in 1..=100 {
            let v = self.api(
                "GET",
                &format!(
                    "{path}{}per_page=100&page={page}",
                    if path.contains('?') { "&" } else { "?" }
                ),
                None,
            )?;
            let rows = v.as_array().ok_or("invalid github list")?;
            all.extend(rows.clone());
            if rows.len() < 100 {
                return Ok(all);
            }
        }
        Err("github pagination limit reached".into())
    }
    pub fn writer(&self, repo: &str, user: &str) -> bool {
        !user.is_empty()
            && user.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            && self
                .api(
                    "GET",
                    &format!("repos/{repo}/collaborators/{user}/permission"),
                    None,
                )
                .is_ok_and(|v| {
                    matches!(
                        v["permission"].as_str(),
                        Some("admin" | "maintain" | "write")
                    )
                })
    }
    pub fn status(&self, repo: &str, number: u64, id: i64, body: &str) -> Result<()> {
        let marker = format!("<!-- shuttle:{id} -->");
        let legacy_marker = format!("<!-- agent-relay:{id} -->");
        let body = format!("{marker}\n{body}");
        let me = self.api("GET", "user", None)?;
        let comments = self.pages(&format!("repos/{repo}/issues/{number}/comments"))?;
        if let Some(comment) = comments.iter().find(|c| {
            c["user"]["login"] == me["login"]
                && c["body"]
                    .as_str()
                    .is_some_and(|s| s.starts_with(&marker) || s.starts_with(&legacy_marker))
        }) {
            let cid = comment["id"].as_u64().ok_or("comment id missing")?;
            self.api(
                "PATCH",
                &format!("repos/{repo}/issues/comments/{cid}"),
                Some(json!({"body":body})),
            )?;
        } else {
            self.api(
                "POST",
                &format!("repos/{repo}/issues/{number}/comments"),
                Some(json!({"body":body})),
            )?;
        }
        Ok(())
    }
}
