//! The Git pane's read: a branch's commits (or every ref's), how many there
//! are in all, the local branches to pick from, and the repository's state
//! around HEAD: changes, upstream, remotes and a command stopped midway.

use std::path::Path;

use gentle_core::git::avatar::github_repo;
use gentle_core::git::log::{Commit, LOG_ARGS, parse_log};
use gentle_core::git::ops::InProgress;
use gentle_core::git::refs::parse_left_right;

use super::git;
use crate::features::backend;

/// Which commits the pane lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    /// HEAD's history.
    #[default]
    Head,
    /// One local branch's history.
    Branch(String),
    /// Every branch, remote branch and tag.
    All,
}

/// One read of the Git pane.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    /// Newest first, at most the limit asked for.
    pub commits: Vec<Commit>,
    /// Every commit the scope reaches, loaded or not.
    pub total: usize,
    /// The scope shown: a branch that no longer exists falls back to HEAD.
    pub scope: Scope,
    /// HEAD's branch; `None` on a detached HEAD.
    pub current: Option<String>,
    /// Local branches, most recently committed first.
    pub branches: std::sync::Arc<[String]>,
    /// The work tree has changes not yet committed. Read when HEAD is shown.
    pub dirty: bool,
    /// HEAD's upstream (`origin/main`) and how far HEAD is `(ahead, behind)` it.
    pub upstream: Option<(String, u32, u32)>,
    /// Remote names.
    pub remotes: Vec<String>,
    /// A merge, rebase, cherry-pick or revert waiting on the user.
    pub in_progress: Option<InProgress>,
    /// `(owner, repository)` when the main remote is on GitHub: its API
    /// names the accounts behind commit authors.
    pub github: Option<(String, String)>,
}

impl History {
    /// The branch the list follows, for the header: HEAD's for HEAD.
    pub fn branch(&self) -> Option<&str> {
        match &self.scope {
            Scope::Branch(name) => Some(name),
            Scope::Head => self.current.as_deref(),
            Scope::All => None,
        }
    }
}

/// The newest `limit` commits `scope` reaches. `None` outside a repository;
/// a repository without commits reads as an empty history.
pub fn history(toplevel: &Path, scope: &Scope, limit: usize) -> Option<History> {
    let current = git(toplevel, &["symbolic-ref", "--short", "-q", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let branches: Vec<String> = git(
        toplevel,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ],
    )?
    .lines()
    .map(str::to_string)
    .collect();
    // A branch deleted since it was picked falls back to HEAD; HEAD's own
    // branch is HEAD.
    let scope = match scope {
        Scope::Branch(name) if Some(name) == current.as_ref() => Scope::Head,
        Scope::Branch(name) if !branches.contains(name) => Scope::Head,
        other => other.clone(),
    };
    let revs: Vec<&str> = match &scope {
        Scope::Head => vec!["HEAD"],
        Scope::Branch(name) => vec![name.as_str()],
        Scope::All => vec!["--all"],
    };
    let limit = format!("-n{limit}");
    let mut args: Vec<&str> = LOG_ARGS.to_vec();
    args.push(limit.as_str());
    args.extend(&revs);
    args.push("--");
    let commits = git(toplevel, &args)
        .map(|raw| parse_log(&raw))
        .unwrap_or_default();
    let mut count = vec!["rev-list", "--count"];
    count.extend(&revs);
    count.push("--");
    let total = git(toplevel, &count)
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(commits.len());
    let dirty = !matches!(scope, Scope::Branch(_))
        && git(
            toplevel,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain",
                "--untracked-files=normal",
            ],
        )
        .is_some_and(|s| !s.trim().is_empty());
    let upstream = git(
        toplevel,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .map(|name| {
        let (ahead, behind) = git(
            toplevel,
            &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )
        .and_then(|raw| parse_left_right(&raw))
        .unwrap_or((0, 0));
        (name, ahead, behind)
    });
    let remotes: Vec<String> = git(toplevel, &["remote"])
        .map(|raw| raw.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let github = remotes
        .iter()
        .find(|r| *r == "origin")
        .or(remotes.first())
        .and_then(|remote| git(toplevel, &["remote", "get-url", remote]))
        .and_then(|url| github_repo(&url));
    Some(History {
        commits,
        total,
        scope,
        current,
        branches: branches.into(),
        dirty,
        upstream,
        remotes,
        github,
        in_progress: in_progress(toplevel),
    })
}

/// The `n` commits after the first `skip` that `tip` reaches, for "Load
/// earlier" to add to a read of `scope` that started at `tip`, without reading
/// again what that read holds. `None` when `scope` has no single tip (every
/// ref) or it no longer starts at `tip` (a new or amended commit, a deleted
/// branch): the page would not continue that read.
pub fn earlier(
    toplevel: &Path,
    scope: &Scope,
    tip: &str,
    skip: usize,
    n: usize,
) -> Option<Vec<Commit>> {
    let rev = match scope {
        Scope::Head => "HEAD",
        Scope::Branch(name) => name.as_str(),
        Scope::All => return None,
    };
    let now = git(
        toplevel,
        &["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")],
    )?;
    if now.trim() != tip {
        return None;
    }
    let skip = format!("--skip={skip}");
    let limit = format!("-n{n}");
    let mut args: Vec<&str> = LOG_ARGS.to_vec();
    args.extend([skip.as_str(), limit.as_str(), tip, "--"]);
    git(toplevel, &args).map(|raw| parse_log(&raw))
}

/// The command git stopped in, from what its directory holds.
pub fn in_progress(toplevel: &Path) -> Option<InProgress> {
    let dir = git(toplevel, &["rev-parse", "--absolute-git-dir"])?;
    let dir = backend::host_path(toplevel, dir.trim());
    InProgress::detect(|name| dir.join(name).exists())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gentle_core::git::log::{append_page, layout};

    use super::*;

    /// A temporary folder, removed when dropped (also when an assertion fails).
    struct TempRepo(PathBuf);

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh repository under the temp folder, named after `test`, whose
    /// history merges and branches: `main` forks `feature` (merged) and
    /// `side` (merged, then tagged `v1`); `wip` forks after `feature`'s merge
    /// with two commits of its own. `None` without git.
    fn branched_repo(test: &str) -> Option<TempRepo> {
        pi_rpc::command("git").arg("--version").output().ok()?;
        let repo = TempRepo(std::env::temp_dir().join(format!(
            "gs-git-{test}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        let dir = &repo.0;
        std::fs::create_dir_all(dir).unwrap();
        let sh = |args: &[&str]| super::super::run(dir, args).unwrap();
        let commit = |file: &str, subject: &str| {
            std::fs::write(dir.join(file), format!("{subject}\n")).unwrap();
            sh(&["add", file]);
            sh(&["commit", "-q", "-m", subject]);
        };
        sh(&["init", "-q", "-b", "main"]);
        sh(&["config", "user.email", "t@example.com"]);
        sh(&["config", "user.name", "T"]);
        sh(&["config", "commit.gpgsign", "false"]);
        commit("a.txt", "m1");
        commit("a.txt", "m2");
        sh(&["checkout", "-q", "-b", "feature"]);
        commit("f.txt", "f1");
        commit("f.txt", "f2");
        sh(&["checkout", "-q", "main"]);
        commit("a.txt", "m3");
        sh(&["checkout", "-q", "-b", "side"]);
        commit("s.txt", "s1");
        sh(&["checkout", "-q", "main"]);
        commit("a.txt", "m4");
        sh(&[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge branch 'feature'",
            "feature",
        ]);
        sh(&["checkout", "-q", "-b", "wip"]);
        commit("w.txt", "w1");
        commit("w.txt", "w2");
        sh(&["checkout", "-q", "main"]);
        commit("a.txt", "m5");
        sh(&[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge branch 'side'",
            "side",
        ]);
        sh(&["tag", "v1"]);
        commit("a.txt", "m6");
        Some(repo)
    }

    /// "Load earlier" adds the page read after the loaded commits from the
    /// commit they start at: for HEAD and for a branch it equals reading
    /// everything again, commit for commit (refs and stats included) and
    /// lane for lane, wherever the page starts.
    #[test]
    fn an_earlier_page_continues_the_loaded_history() {
        let Some(repo) = branched_repo("earlier") else {
            return;
        };
        let dir = &repo.0;
        for scope in [Scope::Head, Scope::Branch("wip".into())] {
            let all = history(dir, &scope, 100).unwrap().commits;
            assert!(all.iter().any(Commit::is_merge), "{scope:?}");
            for loaded in 1..all.len() {
                let read = history(dir, &scope, loaded).unwrap().commits;
                let page = earlier(dir, &scope, &read[0].hash, loaded, 3).unwrap();
                let shown = append_page(&read, loaded, page).unwrap();

                let full = history(dir, &scope, loaded + 3).unwrap().commits;
                assert_eq!(shown, full, "{scope:?} after {loaded}");
                assert_eq!(layout(&shown), layout(&full), "{scope:?} after {loaded}");
            }
        }
    }

    /// A page is read only while the scope still starts at the commit the
    /// loaded list starts at: never for every ref, a deleted branch, an
    /// amended tip or a new commit on it. "Load earlier" then reads
    /// everything again, and that full read starts at the new tip.
    #[test]
    fn no_earlier_page_once_the_tip_moved_or_for_every_ref() {
        let Some(repo) = branched_repo("earlier-moved") else {
            return;
        };
        let dir = &repo.0;
        let sh = |args: &[&str]| super::super::run(dir, args).unwrap();
        let read = history(dir, &Scope::Head, 4).unwrap();
        let tip = read.commits[0].hash.clone();
        assert!(earlier(dir, &Scope::Head, &tip, 4, 3).is_some());
        assert_eq!(earlier(dir, &Scope::All, &tip, 4, 3), None);

        let wip = Scope::Branch("wip".into());
        let wip_tip = history(dir, &wip, 4).unwrap().commits[0].hash.clone();
        sh(&["branch", "-q", "-D", "wip"]);
        assert_eq!(earlier(dir, &wip, &wip_tip, 4, 3), None);

        sh(&["commit", "-q", "--amend", "-m", "m6 amended"]);
        assert_eq!(earlier(dir, &Scope::Head, &tip, 4, 3), None);
        let reread = history(dir, &Scope::Head, 7).unwrap();
        assert_eq!(reread.commits[0].subject, "m6 amended");
        assert_eq!(reread.total, read.total);

        let amended = reread.commits[0].hash.clone();
        std::fs::write(dir.join("a.txt"), "m7\n").unwrap();
        sh(&["commit", "-q", "-am", "m7"]);
        assert_eq!(earlier(dir, &Scope::Head, &amended, 4, 3), None);
        let reread = history(dir, &Scope::Head, 7).unwrap();
        assert_eq!(reread.commits[0].subject, "m7");
        assert_eq!(reread.total, read.total + 1);
    }

    /// Reloading the Git pane reads whether HEAD is dirty: that read must
    /// not rewrite `.git/index` (and take `index.lock`) under the user's git.
    #[test]
    fn history_leaves_a_stat_dirty_index_unwritten() {
        let Some(dir) = super::super::tests::stat_dirty_repo("history") else {
            return;
        };
        let index = std::fs::read(dir.join(".git/index")).unwrap();

        let history = history(&dir, &Scope::Head, 10).unwrap();

        let unchanged = std::fs::read(dir.join(".git/index")).unwrap() == index;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(history.dirty);
        assert_eq!(history.current.as_deref(), Some("main"));
        assert_eq!((history.commits.len(), history.total), (1, 1));
        assert!(unchanged, "history rewrote .git/index");
    }
}
