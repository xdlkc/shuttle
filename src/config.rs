use crate::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const AGENTS: [&str; 5] = ["codex", "antigravity", "pi", "opencode2", "grok"];

#[derive(Clone, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub auto_triage: bool,
    #[serde(default)]
    pub auto_fix: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Agent {
    pub executable: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub repositories: Vec<Repository>,
    pub default_agent: String,
    #[serde(default)]
    pub default_model: Option<String>,
    pub poll_seconds: u64,
    pub timeout_seconds: u64,
    #[serde(default = "auto_limit")]
    pub auto_daily_limit: u32,
    pub agents: BTreeMap<String, Agent>,
}
fn auto_limit() -> u32 {
    20
}
pub fn home() -> PathBuf {
    std::env::var_os("HOME").expect("HOME required").into()
}
pub fn data_dir() -> PathBuf {
    data_dir_for(&home(), cfg!(target_os = "macos"))
}
fn data_dir_for(home: &Path, macos: bool) -> PathBuf {
    let parent = home.join(if macos {
        "Library/Application Support"
    } else {
        ".local/share"
    });
    let legacy = parent.join("AgentRelay");
    // Keep queues, agent runtime and authentication references in place on upgrade.
    if legacy.join("config.json").is_file() {
        legacy
    } else {
        parent.join("Shuttle")
    }
}
pub fn repo_valid(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|s| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
}
pub fn sha_valid(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or("parent missing")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = fs::OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}
pub fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .find(|p| p.is_file())
}
impl Config {
    pub fn initial(repo: String, default_agent: String, auto_triage: bool) -> Result<Self> {
        if !repo_valid(&repo) || !AGENTS.contains(&default_agent.as_str()) {
            return Err("invalid configuration".into());
        }
        let mut agents = BTreeMap::new();
        for name in AGENTS {
            let found = if name == "antigravity" {
                let original = home().join(".local/libexec/agent-defaults/agy-original");
                if original.is_file() {
                    Some(original)
                } else {
                    executable("agy")
                }
            } else {
                executable(name)
            };
            if let Some(executable) = found {
                agents.insert(name.into(), Agent { executable });
            }
        }
        let default_model = if default_agent == "pi" {
            Some("hub/deepseek/deepseek-flash".into())
        } else {
            None
        };
        Ok(Self {
            repositories: vec![Repository {
                name: repo,
                auto_triage,
                auto_fix: false,
            }],
            default_agent,
            default_model,
            poll_seconds: 60,
            timeout_seconds: 1800,
            auto_daily_limit: auto_limit(),
            agents,
        })
    }
    pub fn load(data: &Path) -> Result<Self> {
        let c: Self = serde_json::from_slice(&fs::read(data.join("config.json"))?)?;
        if c.repositories.is_empty()
            || c.repositories.iter().any(|r| !repo_valid(&r.name))
            || !AGENTS.contains(&c.default_agent.as_str())
            || !(30..=3600).contains(&c.poll_seconds)
            || !(30..=7200).contains(&c.timeout_seconds)
            || !(1..=100).contains(&c.auto_daily_limit)
            || c.default_model.as_ref().is_some_and(|s| {
                s.is_empty()
                    || s.len() > 200
                    || s.chars().any(char::is_whitespace)
                    || s.starts_with('-')
            })
        {
            return Err("invalid configuration".into());
        }
        Ok(c)
    }
    pub fn save(&self, data: &Path) -> Result<()> {
        write(&data.join("config.json"), &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renamed_data_root_reuses_legacy_configuration_and_queue_in_place() {
        let home = tempfile::tempdir().unwrap();
        for (macos, parent) in [
            (true, "Library/Application Support"),
            (false, ".local/share"),
        ] {
            assert_eq!(
                data_dir_for(home.path(), macos),
                home.path().join(parent).join("Shuttle")
            );
            let legacy = home.path().join(parent).join("AgentRelay");
            write(&legacy.join("config.json"), b"SYNTHETIC_CONFIGURATION").unwrap();
            assert_eq!(data_dir_for(home.path(), macos), legacy);
            assert!(!home.path().join(parent).join("Shuttle").exists());
        }
    }
    #[test]
    fn validates_control_plane_identifiers() {
        assert!(repo_valid("owner/repo"));
        for s in ["../x", "a/b/c", "a/$(id)", "a/x?token=secret", "a/--x x"] {
            assert!(!repo_valid(s));
        }
        assert!(sha_valid(&"a".repeat(40)));
        assert!(!sha_valid("HEAD"));
    }
}
