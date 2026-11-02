use crate::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub repo: String,
    pub number: u64,
    pub agent: String,
    pub mode: String,
    pub revision: String,
    pub state: String,
    pub request: String,
    pub sha: String,
    pub is_pr: bool,
}
pub struct Queue(pub Connection);
pub struct Link {
    pub task: i64,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub state: String,
}
impl Queue {
    pub fn open(data: &Path) -> Result<Self> {
        std::fs::create_dir_all(data)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = data.join("tasks.sqlite3");
        let c = Connection::open(&path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        c.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE IF NOT EXISTS tasks(id INTEGER PRIMARY KEY,repo TEXT NOT NULL,number INTEGER NOT NULL,agent TEXT NOT NULL,mode TEXT NOT NULL,revision TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'queued',request TEXT NOT NULL,sha TEXT NOT NULL,is_pr INTEGER NOT NULL,created INTEGER NOT NULL DEFAULT(unixepoch()),updated INTEGER NOT NULL DEFAULT(unixepoch()),reason TEXT,UNIQUE(repo,number,agent,mode,revision));")?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS links(task INTEGER PRIMARY KEY,repo TEXT NOT NULL,number INTEGER NOT NULL,url TEXT NOT NULL,state TEXT NOT NULL)")?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS attempts(task INTEGER NOT NULL,started INTEGER NOT NULL DEFAULT(unixepoch()));INSERT INTO attempts(task,started) SELECT id,created FROM tasks WHERE state NOT IN ('queued','blocked','cancelled') AND NOT EXISTS(SELECT 1 FROM attempts WHERE task=tasks.id)")?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS publication_retries(task INTEGER PRIMARY KEY,failures INTEGER NOT NULL)")?;
        Ok(Self(c))
    }
    pub fn insert(&self, t: &Task) -> Result<bool> {
        let number = i64::try_from(t.number)?;
        Ok(self.0.execute("INSERT OR IGNORE INTO tasks(repo,number,agent,mode,revision,request,sha,is_pr) VALUES(?,?,?,?,?,?,?,?)",params![t.repo,number,t.agent,t.mode,t.revision,t.request,t.sha,t.is_pr])?==1)
    }
    pub fn list(&self) -> Result<Vec<Task>> {
        let mut s=self.0.prepare("SELECT id,repo,number,agent,mode,revision,state,request,sha,is_pr FROM tasks ORDER BY id")?;
        Ok(s.query_map([], |r| {
            Ok(Task {
                id: r.get(0)?,
                repo: r.get(1)?,
                number: r
                    .get::<_, i64>(2)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                agent: r.get(3)?,
                mode: r.get(4)?,
                revision: r.get(5)?,
                state: r.get(6)?,
                request: r.get(7)?,
                sha: r.get(8)?,
                is_pr: r.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn state(&self, id: i64, state: &str, reason: &str) -> Result<()> {
        let transaction = self.0.unchecked_transaction()?;
        transaction.execute(
            "UPDATE tasks SET state=?,reason=?,updated=unixepoch() WHERE id=?",
            params![state, reason, id],
        )?;
        if state == "running" {
            transaction.execute("INSERT INTO attempts(task) VALUES(?)", [id])?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn recover(&self) -> Result<()> {
        // Never silently replay a potentially billed generation after a crash.
        self.0.execute("UPDATE tasks SET state='interrupted',reason='worker_restarted',updated=unixepoch() WHERE state='running'",[])?;
        self.0.execute("UPDATE tasks SET state='publication_pending',reason='publication_interrupted',updated=unixepoch() WHERE state='publishing'",[])?;
        Ok(())
    }
    pub fn publication_retry(&self, id: i64) -> Result<bool> {
        self.0.execute("INSERT INTO publication_retries(task,failures) VALUES(?,1) ON CONFLICT(task) DO UPDATE SET failures=failures+1",[id])?;
        let n: i64 = self.0.query_row(
            "SELECT failures FROM publication_retries WHERE task=?",
            [id],
            |r| r.get(0),
        )?;
        Ok(n <= 5)
    }
    pub fn recent_auto_runs(&self) -> Result<u32> {
        Ok(self.0.query_row("SELECT COUNT(*) FROM attempts JOIN tasks ON tasks.id=attempts.task WHERE tasks.revision LIKE 'auto:%' AND attempts.started>unixepoch()-86400",[],|r|r.get(0))?)
    }
    pub fn link(&self, t: &Task, url: &str) -> Result<()> {
        let prefix = format!("https://github.com/{}/pull/", t.repo);
        let number: i64 = url.strip_prefix(&prefix).ok_or("invalid pr url")?.parse()?;
        self.0.execute("INSERT OR REPLACE INTO links(task,repo,number,url,state) VALUES(?,?,?,?,'awaiting_review')",params![t.id,t.repo,number,url])?;
        Ok(())
    }
    pub fn links(&self) -> Result<Vec<Link>> {
        let mut s = self
            .0
            .prepare("SELECT task,repo,number,url,state FROM links")?;
        Ok(s.query_map([], |r| {
            Ok(Link {
                task: r.get(0)?,
                repo: r.get(1)?,
                number: r.get::<_, i64>(2)? as u64,
                url: r.get(3)?,
                state: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn link_state(&self, id: i64, state: &str) -> Result<()> {
        self.0
            .execute("UPDATE links SET state=? WHERE task=?", params![state, id])?;
        self.state(id, state, "")
    }
    pub fn retry(&self, id: i64) -> Result<()> {
        if self.0.execute("UPDATE tasks SET state='queued',reason=NULL WHERE id=? AND state IN ('failed','blocked','interrupted')",[id])?!=1{return Err("task cannot be retried".into())}
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_deduplication_and_crash_recovery() {
        let t = tempfile::tempdir().unwrap();
        let q = Queue::open(t.path()).unwrap();
        let job = Task {
            id: 0,
            repo: "owner/repo".into(),
            number: 1,
            agent: "codex".into(),
            mode: "analyze".into(),
            revision: "fixture".into(),
            state: "queued".into(),
            request: "fixture".into(),
            sha: "a".repeat(40),
            is_pr: false,
        };
        assert!(q.insert(&job).unwrap());
        assert!(!q.insert(&job).unwrap());
        q.state(1, "running", "").unwrap();
        q.recover().unwrap();
        assert_eq!(q.list().unwrap()[0].state, "interrupted");
        q.retry(1).unwrap();
        assert_eq!(q.list().unwrap()[0].state, "queued");
        assert!(crate::config::repo_valid(&job.repo));
        let mut job = job;
        job.id = 1;
        assert!(q.link(&job, "https://evil.invalid/leak").is_err());
        q.link(&job, "https://github.com/owner/repo/pull/7")
            .unwrap();
        assert_eq!(q.links().unwrap()[0].number, 7);
        q.link_state(1, "changes_requested").unwrap();
        assert_eq!(q.list().unwrap()[0].state, "changes_requested");
        q.link_state(1, "merged").unwrap();
        assert_eq!(q.list().unwrap()[0].state, "merged");
        let mut auto = job.clone();
        auto.id = 2;
        auto.revision = "auto:fixture".into();
        q.insert(&auto).unwrap();
        q.state(2, "running", "").unwrap();
        q.state(2, "awaiting_review", "").unwrap();
        assert_eq!(q.recent_auto_runs().unwrap(), 1);
        q.state(2, "publishing", "").unwrap();
        q.recover().unwrap();
        assert_eq!(q.list().unwrap()[1].state, "publication_pending");
        assert_eq!(q.recent_auto_runs().unwrap(), 1);
        for _ in 0..5 {
            assert!(q.publication_retry(2).unwrap());
        }
        assert!(!q.publication_retry(2).unwrap());
    }
}
