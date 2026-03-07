use super::GixRepo;
use crate::util::{run_git_capture, run_git_with_output};
use gitcomet_core::domain::{CommitId, RemoteTag, Tag};
use gitcomet_core::error::{Error, ErrorKind};
use gitcomet_core::services::{CommandOutput, Result};
use gix::bstr::ByteSlice as _;
use std::collections::HashSet;
use std::process::Command;
use std::str;

impl GixRepo {
    pub(super) fn list_tags_impl(&self) -> Result<Vec<Tag>> {
        let repo = self._repo.to_thread_local();

        let refs = repo
            .references()
            .map_err(|e| Error::new(ErrorKind::Backend(format!("gix references: {e}"))))?;

        let iter = refs
            .tags()
            .map_err(|e| Error::new(ErrorKind::Backend(format!("gix tags: {e}"))))?
            .peeled()
            .map_err(|e| Error::new(ErrorKind::Backend(format!("gix peel refs: {e}"))))?;

        let mut tags = Vec::new();
        for reference in iter {
            let reference = reference
                .map_err(|e| Error::new(ErrorKind::Backend(format!("gix ref iter: {e}"))))?;
            let name = reference.name().shorten().to_str_lossy().into_owned();
            let target = CommitId(reference.id().detach().to_string());
            tags.push(Tag { name, target });
        }

        tags.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tags)
    }

    pub(super) fn list_remote_tags_impl(&self) -> Result<Vec<RemoteTag>> {
        let remotes = self.list_remotes_impl()?;
        let mut remote_tags = Vec::new();

        for remote in remotes {
            let mut cmd = Command::new("git");
            cmd.arg("-C")
                .arg(&self.spec.workdir)
                .arg("ls-remote")
                .arg("--tags")
                .arg("--refs")
                .arg(&remote.name);
            let output =
                match run_git_capture(cmd, &format!("git ls-remote --tags --refs {}", remote.name))
                {
                    Ok(output) => output,
                    // Remote tag presence is best-effort metadata for UI menus.
                    // If one remote is unavailable, keep partial results from others.
                    Err(_) => continue,
                };

            for line in output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                let Some((object, reference)) = line.split_once('\t') else {
                    continue;
                };
                let Some(name) = reference.strip_prefix("refs/tags/") else {
                    continue;
                };
                remote_tags.push(RemoteTag {
                    remote: remote.name.clone(),
                    name: name.to_string(),
                    target: CommitId(object.to_string()),
                });
            }
        }

        remote_tags.sort_by(|a, b| {
            a.remote
                .cmp(&b.remote)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.target.as_ref().cmp(b.target.as_ref()))
        });
        Ok(remote_tags)
    }

    pub(super) fn create_tag_with_output_impl(
        &self,
        name: &str,
        target: &str,
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("-c")
            .arg("alias.tag=")
            .arg("-c")
            .arg("tag.gpgsign=false")
            .arg("-c")
            .arg("tag.forcesignannotated=false")
            .arg("tag")
            .arg("-m")
            .arg(name)
            .arg(name)
            .arg(target);
        run_git_with_output(cmd, &format!("git tag -m {name} {name} {target}"))
    }

    pub(super) fn delete_tag_with_output_impl(&self, name: &str) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("-c")
            .arg("alias.tag=")
            .arg("tag")
            .arg("-d")
            .arg(name);
        run_git_with_output(cmd, &format!("git tag -d {name}"))
    }

    pub(super) fn push_tag_with_output_impl(
        &self,
        remote: &str,
        name: &str,
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("push")
            .arg(remote)
            .arg(format!("refs/tags/{name}"));
        run_git_with_output(cmd, &format!("git push {remote} refs/tags/{name}"))
    }

    pub(super) fn delete_remote_tag_with_output_impl(
        &self,
        remote: &str,
        name: &str,
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.spec.workdir)
            .arg("push")
            .arg(remote)
            .arg("--delete")
            .arg(format!("refs/tags/{name}"));
        run_git_with_output(cmd, &format!("git push {remote} --delete refs/tags/{name}"))
    }

    pub(super) fn prune_local_tags_with_output_impl(&self) -> Result<CommandOutput> {
        let remotes = self.list_remotes_impl()?;
        if remotes.is_empty() {
            return Ok(CommandOutput {
                command: "git prune local tags".to_string(),
                stdout: "No remotes configured; skipping tag prune.\n".to_string(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }

        let mut remote_tags: HashSet<String> = HashSet::new();
        for remote in remotes {
            let mut cmd = Command::new("git");
            cmd.arg("-C")
                .arg(&self.spec.workdir)
                .arg("ls-remote")
                .arg("--tags")
                .arg("--refs")
                .arg(&remote.name);
            let output =
                run_git_capture(cmd, &format!("git ls-remote --tags --refs {}", remote.name))?;
            for line in output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                let Some((_object, reference)) = line.split_once('\t') else {
                    continue;
                };
                let Some(name) = reference.strip_prefix("refs/tags/") else {
                    continue;
                };
                remote_tags.insert(name.to_string());
            }
        }

        let mut list_cmd = Command::new("git");
        list_cmd
            .arg("-C")
            .arg(&self.spec.workdir)
            .arg("tag")
            .arg("--list");
        let local_tags = run_git_capture(list_cmd, "git tag --list")?;

        let mut deleted: Vec<String> = Vec::new();
        let mut deleted_outputs: Vec<CommandOutput> = Vec::new();
        for local_tag in local_tags
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if remote_tags.contains(local_tag) {
                continue;
            }
            let mut delete_cmd = Command::new("git");
            delete_cmd
                .arg("-C")
                .arg(&self.spec.workdir)
                .arg("-c")
                .arg("alias.tag=")
                .arg("tag")
                .arg("-d")
                .arg(local_tag);
            let output = run_git_with_output(delete_cmd, &format!("git tag -d {local_tag}"))?;
            deleted.push(local_tag.to_string());
            deleted_outputs.push(output);
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        for output in &deleted_outputs {
            if !output.stdout.is_empty() {
                stdout.push_str(&output.stdout);
            }
            if !output.stderr.is_empty() {
                stderr.push_str(&output.stderr);
            }
        }
        if deleted.is_empty() {
            stdout.push_str("No local tags to prune.\n");
        } else {
            if !stdout.ends_with('\n') && !stdout.is_empty() {
                stdout.push('\n');
            }
            stdout.push_str("Pruned local tags:\n");
            for name in deleted {
                stdout.push_str("- ");
                stdout.push_str(&name);
                stdout.push('\n');
            }
        }

        Ok(CommandOutput {
            command: "git prune local tags".to_string(),
            stdout,
            stderr,
            exit_code: Some(0),
        })
    }
}
