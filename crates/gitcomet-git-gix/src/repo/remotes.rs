use super::GixRepo;
use crate::util::{parse_remote_branches, run_git_capture, run_git_simple, run_git_with_output};
use gitcomet_core::domain::{Remote, RemoteBranch};
use gitcomet_core::error::{Error, ErrorKind};
use gitcomet_core::services::{CommandOutput, PullMode, RemoteUrlKind, Result};
use gix::bstr::ByteSlice as _;
use std::collections::HashSet;
use std::process::Command;
use std::str;

impl GixRepo {
    fn preferred_remote_name(&self) -> Result<Option<String>> {
        let remotes = self.list_remotes_impl()?;
        if remotes.is_empty() {
            return Ok(None);
        }
        if remotes.iter().any(|r| r.name == "origin") {
            return Ok(Some("origin".to_string()));
        }
        Ok(Some(remotes[0].name.clone()))
    }

    fn current_branch_name(&self) -> Result<Option<String>> {
        let head = self.current_branch_impl()?;
        let head = head.trim();
        if head.is_empty() || head == "HEAD" {
            return Ok(None);
        }
        Ok(Some(head.to_string()))
    }

    fn branch_has_upstream(&self, branch: &str) -> Result<bool> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("for-each-ref")
            .arg("--format=%(upstream:short)")
            .arg(format!("refs/heads/{branch}"));
        Ok(!run_git_capture(cmd, "git for-each-ref refs/heads")?
            .trim()
            .is_empty())
    }

    pub(super) fn list_remotes_impl(&self) -> Result<Vec<Remote>> {
        let repo = self._repo.to_thread_local();
        let mut remotes = Vec::new();

        for name in repo.remote_names() {
            let remote = repo
                .find_remote(name.as_ref())
                .map_err(|e| Error::new(ErrorKind::Backend(format!("gix find_remote: {e}"))))?;

            let url = remote
                .url(gix::remote::Direction::Fetch)
                .map(|u| u.to_string());

            remotes.push(Remote {
                name: name.to_str_lossy().into_owned(),
                url,
            });
        }

        remotes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(remotes)
    }

    pub(super) fn list_remote_branches_impl(&self) -> Result<Vec<RemoteBranch>> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("for-each-ref")
            .arg("--format=%(refname:strip=2)\t%(objectname)")
            .arg("refs/remotes");
        let output = run_git_capture(cmd, "git for-each-ref refs/remotes")?;
        Ok(parse_remote_branches(&output))
    }

    pub(super) fn fetch_all_impl(&self, prune: bool) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("fetch")
            .arg("--all");
        if prune {
            cmd.arg("--prune");
        }
        run_git_simple(
            cmd,
            if prune {
                "git fetch --all --prune"
            } else {
                "git fetch --all"
            },
        )
    }

    pub(super) fn fetch_all_with_output_impl(&self, prune: bool) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("fetch")
            .arg("--all");
        if prune {
            cmd.arg("--prune");
        }
        run_git_with_output(
            cmd,
            if prune {
                "git fetch --all --prune"
            } else {
                "git fetch --all"
            },
        )
    }

    pub(super) fn pull_impl(&self, mode: PullMode) -> Result<()> {
        let branch = self.current_branch_name()?;
        let has_upstream = match branch.as_deref() {
            Some(branch) => self.branch_has_upstream(branch)?,
            None => true,
        };

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.spec.workdir).arg("pull");
        match mode {
            // Be explicit about ff behavior so we don't create merge commits when a fast-forward
            // is possible, even if the user's git config disables ff.
            PullMode::Default => {
                cmd.arg("--ff");
            }
            PullMode::Merge => {
                cmd.arg("--no-rebase");
                cmd.arg("--ff");
            }
            PullMode::FastForwardIfPossible => {
                cmd.arg("--ff");
            }
            PullMode::FastForwardOnly => {
                cmd.arg("--ff-only");
            }
            PullMode::Rebase => {
                cmd.arg("--rebase");
            }
        }

        if !has_upstream
            && let Some(branch) = branch
            && let Some(remote) = self.preferred_remote_name()?
        {
            cmd.arg(&remote).arg(&branch);
            run_git_simple(cmd, &format!("git pull {remote} {branch}"))?;

            let mut set_upstream = Command::new("git");
            set_upstream
                .arg("-C")
                .arg(&self.spec.workdir)
                .arg("branch")
                .arg("--set-upstream-to")
                .arg(format!("{remote}/{branch}"))
                .arg(branch);
            run_git_simple(set_upstream, "git branch --set-upstream-to")?;
            return Ok(());
        }

        run_git_simple(cmd, "git pull")
    }

    pub(super) fn pull_with_output_impl(&self, mode: PullMode) -> Result<CommandOutput> {
        let branch = self.current_branch_name()?;
        let has_upstream = match branch.as_deref() {
            Some(branch) => self.branch_has_upstream(branch)?,
            None => true,
        };

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.spec.workdir).arg("pull");
        match mode {
            // Be explicit about ff behavior so we don't create merge commits when a fast-forward
            // is possible, even if the user's git config disables ff.
            PullMode::Default => {
                cmd.arg("--ff");
            }
            PullMode::Merge => {
                cmd.arg("--no-rebase");
                cmd.arg("--ff");
            }
            PullMode::FastForwardIfPossible => {
                cmd.arg("--ff");
            }
            PullMode::FastForwardOnly => {
                cmd.arg("--ff-only");
            }
            PullMode::Rebase => {
                cmd.arg("--rebase");
            }
        }

        if !has_upstream
            && let Some(branch) = branch
            && let Some(remote) = self.preferred_remote_name()?
        {
            cmd.arg(&remote).arg(&branch);
            let output = run_git_with_output(cmd, &format!("git pull {remote} {branch}"))?;

            let mut set_upstream = Command::new("git");
            set_upstream
                .arg("-C")
                .arg(&self.spec.workdir)
                .arg("branch")
                .arg("--set-upstream-to")
                .arg(format!("{remote}/{branch}"))
                .arg(branch);
            run_git_simple(set_upstream, "git branch --set-upstream-to")?;
            return Ok(output);
        }

        run_git_with_output(cmd, "git pull")
    }

    pub(super) fn push_impl(&self) -> Result<()> {
        if let Some(branch) = self.current_branch_name()?
            && !self.branch_has_upstream(&branch)?
            && let Some(remote) = self.preferred_remote_name()?
        {
            return self.push_set_upstream_impl(&remote, &branch);
        }

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.spec.workdir).arg("push");
        run_git_simple(cmd, "git push")
    }

    pub(super) fn push_with_output_impl(&self) -> Result<CommandOutput> {
        if let Some(branch) = self.current_branch_name()?
            && !self.branch_has_upstream(&branch)?
            && let Some(remote) = self.preferred_remote_name()?
        {
            return self.push_set_upstream_with_output_impl(&remote, &branch);
        }

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.spec.workdir).arg("push");
        run_git_with_output(cmd, "git push")
    }

    pub(super) fn push_force_impl(&self) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("push")
            .arg("--force-with-lease");
        run_git_simple(cmd, "git push --force-with-lease")
    }

    pub(super) fn push_force_with_output_impl(&self) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("push")
            .arg("--force-with-lease");
        run_git_with_output(cmd, "git push --force-with-lease")
    }

    pub(super) fn pull_branch_with_output_impl(
        &self,
        remote: &str,
        branch: &str,
    ) -> Result<CommandOutput> {
        let command_str = format!("git pull --no-rebase --ff {remote} {branch}");
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("-c")
            .arg("color.ui=false")
            .arg("--no-pager")
            .arg("pull")
            .arg("--no-rebase")
            .arg("--ff")
            .arg(remote)
            .arg(branch);
        run_git_with_output(cmd, &command_str)
    }

    pub(super) fn merge_ref_with_output_impl(&self, reference: &str) -> Result<CommandOutput> {
        let command_str = format!("git merge --ff --no-edit {reference}");
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("-c")
            .arg("color.ui=false")
            .arg("--no-pager")
            .arg("merge")
            .arg("--ff")
            .arg("--no-edit")
            .arg(reference);
        run_git_with_output(cmd, &command_str)
    }

    pub(super) fn add_remote_with_output_impl(
        &self,
        name: &str,
        url: &str,
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("remote")
            .arg("add")
            .arg(name)
            .arg(url);
        run_git_with_output(cmd, &format!("git remote add {name} {url}"))
    }

    pub(super) fn remove_remote_with_output_impl(&self, name: &str) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("remote")
            .arg("remove")
            .arg(name);
        run_git_with_output(cmd, &format!("git remote remove {name}"))
    }

    pub(super) fn set_remote_url_with_output_impl(
        &self,
        name: &str,
        url: &str,
        kind: RemoteUrlKind,
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("remote")
            .arg("set-url");
        match kind {
            RemoteUrlKind::Fetch => {}
            RemoteUrlKind::Push => {
                cmd.arg("--push");
            }
        }
        cmd.arg(name).arg(url);
        let label = match kind {
            RemoteUrlKind::Fetch => format!("git remote set-url {name} {url}"),
            RemoteUrlKind::Push => format!("git remote set-url --push {name} {url}"),
        };
        run_git_with_output(cmd, &label)
    }

    pub(super) fn push_set_upstream_impl(&self, remote: &str, branch: &str) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("push")
            .arg("--set-upstream")
            .arg(remote)
            .arg(format!("HEAD:refs/heads/{branch}"));
        run_git_simple(
            cmd,
            &format!("git push --set-upstream {remote} HEAD:refs/heads/{branch}"),
        )
    }

    pub(super) fn push_set_upstream_with_output_impl(
        &self,
        remote: &str,
        branch: &str,
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("push")
            .arg("--set-upstream")
            .arg(remote)
            .arg(format!("HEAD:refs/heads/{branch}"));
        run_git_with_output(
            cmd,
            &format!("git push --set-upstream {remote} HEAD:refs/heads/{branch}"),
        )
    }

    pub(super) fn delete_remote_branch_with_output_impl(
        &self,
        remote: &str,
        branch: &str,
    ) -> Result<CommandOutput> {
        let label = format!("git push {remote} --delete {branch}");
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("push")
            .arg(remote)
            .arg("--delete")
            .arg(branch);
        let output = run_git_with_output(cmd, &label)?;

        let refname = format!("refs/remotes/{remote}/{branch}");
        let mut prune = Command::new("git");
        prune
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("update-ref")
            .arg("-d")
            .arg(refname);
        let _ = prune.output();

        Ok(output)
    }

    pub(super) fn prune_merged_branches_with_output_impl(&self) -> Result<CommandOutput> {
        let fetch_output = self.fetch_all_with_output_impl(true)?;

        let mut merged_cmd = Command::new("git");
        merged_cmd
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("for-each-ref")
            .arg("--format=%(refname:short)")
            .arg("--merged=HEAD")
            .arg("refs/heads");
        let merged_output =
            run_git_capture(merged_cmd, "git for-each-ref --merged=HEAD refs/heads")?;
        let merged: HashSet<String> = merged_output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        let mut branches_cmd = Command::new("git");
        branches_cmd
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("for-each-ref")
            .arg("--format=%(refname:short)\t%(upstream:short)")
            .arg("refs/heads");
        let branches_output = run_git_capture(
            branches_cmd,
            "git for-each-ref --format=%(refname:short)\\t%(upstream:short) refs/heads",
        )?;

        let current_branch = self.current_branch_name()?;
        let mut deleted: Vec<String> = Vec::new();
        let mut deleted_outputs: Vec<CommandOutput> = Vec::new();

        for line in branches_output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let (branch, upstream) = line.split_once('\t').unwrap_or((line, ""));
            if branch.is_empty() || upstream.is_empty() {
                continue;
            }
            if current_branch.as_deref() == Some(branch) {
                continue;
            }
            if !merged.contains(branch) {
                continue;
            }

            let tracking_ref = format!("refs/remotes/{upstream}");
            let tracking_exists = Command::new("git")
                .arg("-C")
                .arg(&self.spec.workdir)
                .arg("show-ref")
                .arg("--verify")
                .arg("--quiet")
                .arg(&tracking_ref)
                .output()
                .map_err(|e| Error::new(ErrorKind::Io(e.kind())))?
                .status
                .success();

            if tracking_exists {
                continue;
            }

            let mut delete_cmd = Command::new("git");
            delete_cmd
                .arg("-C")
                .arg(&self.spec.workdir)
                .arg("branch")
                .arg("-d")
                .arg(branch);
            let output = run_git_with_output(delete_cmd, &format!("git branch -d {branch}"))?;
            deleted.push(branch.to_string());
            deleted_outputs.push(output);
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        if !fetch_output.stdout.is_empty() {
            stdout.push_str(&fetch_output.stdout);
        }
        if !fetch_output.stderr.is_empty() {
            stderr.push_str(&fetch_output.stderr);
        }
        for output in &deleted_outputs {
            if !output.stdout.is_empty() {
                stdout.push_str(&output.stdout);
            }
            if !output.stderr.is_empty() {
                stderr.push_str(&output.stderr);
            }
        }
        if deleted.is_empty() {
            if !stdout.ends_with('\n') && !stdout.is_empty() {
                stdout.push('\n');
            }
            stdout.push_str("No merged local branches to prune.\n");
        } else {
            if !stdout.ends_with('\n') && !stdout.is_empty() {
                stdout.push('\n');
            }
            stdout.push_str("Pruned merged local branches:\n");
            for branch in deleted {
                stdout.push_str("- ");
                stdout.push_str(&branch);
                stdout.push('\n');
            }
        }

        Ok(CommandOutput {
            command: "git prune merged branches".to_string(),
            stdout,
            stderr,
            exit_code: Some(0),
        })
    }
}
