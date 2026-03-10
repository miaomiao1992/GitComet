use super::GixRepo;
use crate::util::{run_git_with_output, validate_ref_like_arg};
use gitcomet_core::error::{Error, ErrorKind};
use gitcomet_core::services::{CommandOutput, ResetMode, Result};
use std::path::PathBuf;
use std::process::Command;
use std::str;

fn path_from_git_stdout(stdout: &[u8]) -> Option<PathBuf> {
    let raw = stdout.trim_ascii_end();
    if raw.is_empty() {
        return None;
    }

    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        Some(PathBuf::from(OsString::from_vec(raw.to_vec())))
    }

    #[cfg(windows)]
    {
        let text = std::str::from_utf8(raw).ok()?;
        if text.is_empty() {
            None
        } else {
            Some(PathBuf::from(text))
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let text = std::str::from_utf8(raw).ok()?;
        if text.is_empty() {
            None
        } else {
            Some(PathBuf::from(text))
        }
    }
}

impl GixRepo {
    pub(super) fn reset_with_output_impl(
        &self,
        target: &str,
        mode: ResetMode,
    ) -> Result<CommandOutput> {
        validate_ref_like_arg(target, "reset target")?;

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.spec.workdir).arg("reset");
        let mode_flag = match mode {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        };
        cmd.arg(mode_flag).arg(target);
        let label = format!("git reset {mode_flag} {target}");
        run_git_with_output(cmd, &label)
    }

    pub(super) fn rebase_with_output_impl(&self, onto: &str) -> Result<CommandOutput> {
        validate_ref_like_arg(onto, "rebase target")?;

        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("rebase")
            .arg("--")
            .arg(onto);
        run_git_with_output(cmd, &format!("git rebase {onto}"))
    }

    pub(super) fn rebase_continue_with_output_impl(&self) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("rebase")
            .arg("--continue");
        run_git_with_output(cmd, "git rebase --continue")
    }

    pub(super) fn rebase_abort_with_output_impl(&self) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("rebase")
            .arg("--abort");
        match run_git_with_output(cmd, "git rebase --abort") {
            Ok(output) => Ok(output),
            Err(rebase_error) => {
                // `git am` uses its own sequencer state. Falling back here allows a
                // single "abort in-progress operation" UI action to handle both rebase
                // and patch-apply flows.
                let mut am_cmd = Command::new("git");
                am_cmd
                    .arg("-C")
                    .arg(&self.spec.workdir)
                    .arg("am")
                    .arg("--abort");
                match run_git_with_output(am_cmd, "git am --abort") {
                    Ok(output) => Ok(output),
                    Err(_) => Err(rebase_error),
                }
            }
        }
    }

    pub(super) fn merge_abort_with_output_impl(&self) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("merge")
            .arg("--abort");
        run_git_with_output(cmd, "git merge --abort")
    }

    pub(super) fn rebase_in_progress_impl(&self) -> Result<bool> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("rev-parse")
            .arg("--verify")
            .arg("REBASE_HEAD")
            .output()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;
        if output.status.success() {
            return Ok(true);
        }

        // `git am` tracks progress in `.git/rebase-apply` and does not set REBASE_HEAD.
        let applying_path = Command::new("git")
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("rev-parse")
            .arg("--git-path")
            .arg("rebase-apply/applying")
            .output()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;

        if !applying_path.status.success() {
            return Ok(false);
        }

        let Some(applying_path) = path_from_git_stdout(&applying_path.stdout) else {
            return Ok(false);
        };
        let applying_path = if applying_path.is_absolute() {
            applying_path
        } else {
            self.spec.workdir.join(applying_path)
        };

        Ok(applying_path.exists())
    }

    pub(super) fn merge_commit_message_impl(&self) -> Result<Option<String>> {
        let merge_head = Command::new("git")
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("rev-parse")
            .arg("--verify")
            .arg("MERGE_HEAD")
            .output()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;

        if !merge_head.status.success() {
            return Ok(None);
        }

        let merge_msg_path = Command::new("git")
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("rev-parse")
            .arg("--git-path")
            .arg("MERGE_MSG")
            .output()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;

        if !merge_msg_path.status.success() {
            let stderr = str::from_utf8(&merge_msg_path.stderr).unwrap_or("<non-utf8 stderr>");
            return Err(Error::new(ErrorKind::Backend(format!(
                "git rev-parse --git-path MERGE_MSG failed: {}",
                stderr.trim()
            ))));
        }

        let Some(merge_msg_path) = path_from_git_stdout(&merge_msg_path.stdout) else {
            return Ok(None);
        };
        let merge_msg_path = if merge_msg_path.is_absolute() {
            merge_msg_path
        } else {
            self.spec.workdir.join(merge_msg_path)
        };

        let contents = match std::fs::read_to_string(&merge_msg_path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::new(ErrorKind::Io(e.kind()))),
        };

        let mut lines: Vec<&str> = Vec::new();
        for line in contents.lines() {
            let line = line.trim_end();
            if line.trim_start().starts_with('#') {
                continue;
            }
            lines.push(line);
        }

        let Some(start) = lines.iter().position(|l| !l.trim().is_empty()) else {
            return Ok(None);
        };
        let end = lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map(|ix| ix + 1)
            .unwrap_or(start + 1);

        let message = lines[start..end].join("\n");
        if message.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(message))
        }
    }
}
