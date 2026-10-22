//! Git through the `git` command line: the repository of a folder, file
//! status and committed contents. Everything degrades to "not a repository"
//! when git is missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl FileStatus {
    pub fn letter(self) -> &'static str {
        match self {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Untracked => "U",
            FileStatus::Conflicted => "!",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FileStatus::Modified => "modified",
            FileStatus::Added => "added",
            FileStatus::Deleted => "deleted",
            FileStatus::Renamed => "renamed",
            FileStatus::Untracked => "untracked",
            FileStatus::Conflicted => "conflict",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Repo {
    pub root: PathBuf,
    pub branch: String,
    /// Changed files by absolute path.
    pub status: HashMap<PathBuf, FileStatus>,
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = crate::desktop::command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// The repository containing `dir` with its current status, if any.
pub fn open(dir: &Path) -> Option<Repo> {
    let root = git(dir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(String::from_utf8_lossy(&root).trim());
    let branch = git(&root, &["branch", "--show-current"])
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "detached".into());
    let raw = git(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Some(Repo {
        status: parse_status(&root, &raw),
        root,
        branch,
    })
}

fn parse_status(root: &Path, raw: &[u8]) -> HashMap<PathBuf, FileStatus> {
    let mut out = HashMap::new();
    let mut entries = raw.split(|b| *b == 0).filter(|e| e.len() > 3);
    while let Some(entry) = entries.next() {
        let (x, y) = (entry[0], entry[1]);
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let status = match (x, y) {
            (b'?', b'?') => FileStatus::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => FileStatus::Conflicted,
            (b'R', _) | (b'C', _) => {
                // The original path follows as its own entry.
                entries.next();
                FileStatus::Renamed
            }
            (b'A', _) => FileStatus::Added,
            (b'D', _) | (_, b'D') => FileStatus::Deleted,
            _ => FileStatus::Modified,
        };
        out.insert(root.join(path), status);
    }
    out
}

/// The committed (HEAD) contents of a file; `None` if it isn't committed.
pub fn head_text(root: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(root).ok()?;
    let spec = format!("HEAD:{}", relative.to_string_lossy().replace('\\', "/"));
    git(root, &["show", &spec]).map(|b| crate::text_file::decode(&b).text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_status() {
        let raw =
            b" M pkg/a.pkb\0?? new.sql\0A  added.sql\0R  new_name.sql\0old_name.sql\0 D gone.sql\0";
        let s = parse_status(Path::new("/r"), raw);
        assert_eq!(s[Path::new("/r/pkg/a.pkb")], FileStatus::Modified);
        assert_eq!(s[Path::new("/r/new.sql")], FileStatus::Untracked);
        assert_eq!(s[Path::new("/r/added.sql")], FileStatus::Added);
        assert_eq!(s[Path::new("/r/new_name.sql")], FileStatus::Renamed);
        assert_eq!(s[Path::new("/r/gone.sql")], FileStatus::Deleted);
        assert!(!s.contains_key(Path::new("/r/old_name.sql")));
    }

    #[test]
    fn reads_a_real_repository() {
        if crate::desktop::command("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("pythia-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| assert!(git(&dir, args).is_some(), "git {args:?}");
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(dir.join("a.sql"), "select 1 from dual;\n").unwrap();
        run(&["add", "a.sql"]);
        run(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("a.sql"), "select 2 from dual;\n").unwrap();
        std::fs::write(dir.join("b.sql"), "x").unwrap();
        let repo = open(&dir).unwrap();
        let root = repo.root.clone();
        assert_eq!(repo.branch, "main");
        assert_eq!(repo.status[&root.join("a.sql")], FileStatus::Modified);
        assert_eq!(repo.status[&root.join("b.sql")], FileStatus::Untracked);
        assert_eq!(
            head_text(&root, &root.join("a.sql")).unwrap(),
            "select 1 from dual;\n"
        );
        assert!(head_text(&root, &root.join("b.sql")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
