use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

/// How a file has changed relative to the git index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    New,
    Modified,
    Deleted,
}

/// A file that has changed in the working tree or index.
#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub change: FileChange,
}

// TODO(git-env-leaks-into-test-repos): every git spawn in this file inherits
// the caller's GIT_* environment.

/// Returns the SHA of HEAD.
///
/// # Panics
///
/// Panics if the repo has no commits — graf requires a valid repo with at
/// least one commit.  An empty `git init` with no commits is a setup error.
pub fn rev_parse_head(repo_dir: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .context("failed to run git rev-parse HEAD")?;

    assert!(
        output.status.success(),
        "git rev-parse HEAD failed — graf requires a repo with at least one commit: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let sha = String::from_utf8(output.stdout)
        .context("git rev-parse HEAD output is not UTF-8")?
        .trim()
        .to_string();

    ensure!(!sha.is_empty(), "git rev-parse HEAD returned empty string");
    Ok(sha)
}

/// Returns files changed between two commits.
///
/// Uses `git diff --name-status <from> <to>`.  Renames are returned as TWO
/// entries: a `Deleted` for the old path and a `New` for the new path.
pub fn diff_between(repo_dir: &Path, from: &str, to: &str) -> Result<Vec<ChangedFile>> {
    let output = std::process::Command::new("git")
        .args(["diff", "--name-status", from, to])
        .current_dir(repo_dir)
        .output()
        .context("failed to run git diff --name-status")?;

    ensure!(
        output.status.success(),
        "git diff --name-status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).context("git diff output is not UTF-8")?;
    parse_name_status(&stdout)
}

/// Returns true if `ancestor` is an ancestor of `descendant`.
pub fn is_ancestor(repo_dir: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo_dir)
        .output()
        .context("failed to run git merge-base --is-ancestor")?;

    // Exit code 0 = yes (is ancestor), 1 = no (not ancestor)
    // Other exit codes = actual error
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!(
            "git merge-base --is-ancestor failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

/// Run `git status --porcelain` in `repo_dir` and return all changed files.
///
/// Returns both `.md` and non-`.md` files so callers can check for orphan
/// attachments. Callers filter by extension as needed.
pub fn changed_files(repo_dir: &Path) -> Result<Vec<ChangedFile>> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .context("failed to run git status")?;

    ensure!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).context("git status output is not UTF-8")?;
    parse_porcelain(&stdout)
}

/// Parse `git status --porcelain` (v1) output into a list of changed files.
///
/// Format: two status columns (index, worktree), a space, then the path.
/// For renames: `R  new_path -> old_path`.
fn parse_porcelain(output: &str) -> Result<Vec<ChangedFile>> {
    let mut files = Vec::new();

    for line in output.lines() {
        if line.len() < 4 {
            continue;
        }

        let index = line.as_bytes()[0];
        let worktree = line.as_bytes()[1];
        let path_str = &line[3..];

        // Renames are expanded into Deleted(old) + New(new), same as
        // parse_name_status does for commit diffs.  The sync protocol needs
        // both entries so it can remove the old path's DB row.
        if index == b'R' || worktree == b'R' {
            if let Some((old, new)) = path_str.split_once(" -> ") {
                files.push(ChangedFile {
                    path: PathBuf::from(old),
                    change: FileChange::Deleted,
                });
                files.push(ChangedFile {
                    path: PathBuf::from(new),
                    change: FileChange::New,
                });
            }
            continue;
        }

        let change = match (index, worktree) {
            (b'?', b'?') => FileChange::New,
            (b'A', _) => FileChange::New,
            (b'D', _) | (_, b'D') => FileChange::Deleted,
            (b'M', _) | (_, b'M') => FileChange::Modified,
            // Any other combination (e.g., 'C' for copy) treated as Modified
            _ => FileChange::Modified,
        };

        files.push(ChangedFile {
            path: PathBuf::from(path_str),
            change,
        });
    }

    Ok(files)
}

/// Parse `git diff --name-status` output into a list of changed files.
///
/// Format: `<status>\t<path>` or `R<pct>\t<old>\t<new>` for renames.
/// Renames are expanded into two entries: Deleted(old) + New(new).
fn parse_name_status(output: &str) -> Result<Vec<ChangedFile>> {
    let mut files = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split('\t').collect();
        ensure!(
            parts.len() >= 2,
            "unexpected git diff --name-status line: {line}"
        );

        let status = parts[0];
        if status.starts_with('R') {
            // Rename: R<pct>\t<old>\t<new>
            ensure!(parts.len() >= 3, "rename line missing new path: {line}");
            files.push(ChangedFile {
                path: PathBuf::from(parts[1]),
                change: FileChange::Deleted,
            });
            files.push(ChangedFile {
                path: PathBuf::from(parts[2]),
                change: FileChange::New,
            });
        } else {
            let change = match status {
                "A" => FileChange::New,
                "D" => FileChange::Deleted,
                "M" => FileChange::Modified,
                // C (copy), T (type change), etc. — treat as Modified
                _ => FileChange::Modified,
            };
            files.push(ChangedFile {
                path: PathBuf::from(parts[1]),
                change,
            });
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_untracked() {
        let output = "?? new_file.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("new_file.md"));
        assert_eq!(files[0].change, FileChange::New);
    }

    #[test]
    fn test_parse_added() {
        let output = "A  staged_new.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::New);
    }

    #[test]
    fn test_parse_modified_worktree() {
        let output = " M modified.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::Modified);
    }

    #[test]
    fn test_parse_modified_staged() {
        let output = "M  staged_modified.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::Modified);
    }

    #[test]
    fn test_parse_modified_both() {
        let output = "MM both_modified.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::Modified);
    }

    #[test]
    fn test_parse_deleted_worktree() {
        let output = " D deleted.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::Deleted);
    }

    #[test]
    fn test_parse_deleted_staged() {
        let output = "D  deleted.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::Deleted);
    }

    #[test]
    fn test_parse_renamed() {
        let output = "R  old_name.md -> new_name.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("old_name.md"));
        assert_eq!(files[0].change, FileChange::Deleted);
        assert_eq!(files[1].path, PathBuf::from("new_name.md"));
        assert_eq!(files[1].change, FileChange::New);
    }

    #[test]
    fn test_parse_mixed() {
        let output = "\
?? untracked.md
A  added.md
 M modified.md
D  deleted.md
MM both.md
R  old.md -> renamed.md
?? notes.txt
";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files.len(), 8);
        assert_eq!(files[0].change, FileChange::New); // untracked.md
        assert_eq!(files[1].change, FileChange::New); // added.md
        assert_eq!(files[2].change, FileChange::Modified); // modified.md
        assert_eq!(files[3].change, FileChange::Deleted); // deleted.md
        assert_eq!(files[4].change, FileChange::Modified); // both.md
        assert_eq!(files[5].change, FileChange::Deleted); // old.md (rename)
        assert_eq!(files[6].change, FileChange::New); // renamed.md (rename)
        assert_eq!(files[7].change, FileChange::New); // notes.txt
    }

    #[test]
    fn test_parse_empty() {
        let files = parse_porcelain("").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_paths_with_subdirectories() {
        let output = "?? tech/backup/new_doc.md\n M life/todo/task.md\n";
        let files = parse_porcelain(output).unwrap();
        assert_eq!(files[0].path, PathBuf::from("tech/backup/new_doc.md"));
        assert_eq!(files[1].path, PathBuf::from("life/todo/task.md"));
    }

    // --- parse_name_status tests ---

    #[test]
    fn test_name_status_added() {
        let output = "A\tnew_file.md\n";
        let files = parse_name_status(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("new_file.md"));
        assert_eq!(files[0].change, FileChange::New);
    }

    #[test]
    fn test_name_status_modified() {
        let output = "M\tmodified.md\n";
        let files = parse_name_status(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::Modified);
    }

    #[test]
    fn test_name_status_deleted() {
        let output = "D\tremoved.md\n";
        let files = parse_name_status(output).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].change, FileChange::Deleted);
    }

    #[test]
    fn test_name_status_rename_expands_to_two() {
        let output = "R100\told_name.md\tnew_name.md\n";
        let files = parse_name_status(output).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("old_name.md"));
        assert_eq!(files[0].change, FileChange::Deleted);
        assert_eq!(files[1].path, PathBuf::from("new_name.md"));
        assert_eq!(files[1].change, FileChange::New);
    }

    #[test]
    fn test_name_status_mixed() {
        let output = "A\tadded.md\nM\tchanged.md\nD\tgone.md\nR090\told.md\tnew.md\n";
        let files = parse_name_status(output).unwrap();
        assert_eq!(files.len(), 5);
        assert_eq!(files[0].change, FileChange::New); // added
        assert_eq!(files[1].change, FileChange::Modified); // changed
        assert_eq!(files[2].change, FileChange::Deleted); // gone
        assert_eq!(files[3].change, FileChange::Deleted); // old (rename)
        assert_eq!(files[4].change, FileChange::New); // new (rename)
    }

    #[test]
    fn test_name_status_empty() {
        let files = parse_name_status("").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_name_status_subdirectories() {
        let output = "M\ttodo/hvac-filters.md\nA\tkb/new-topic.md\n";
        let files = parse_name_status(output).unwrap();
        assert_eq!(files[0].path, PathBuf::from("todo/hvac-filters.md"));
        assert_eq!(files[1].path, PathBuf::from("kb/new-topic.md"));
    }
}
