use std::sync::{Mutex, OnceLock};

pub const GITCOMET_AUTH_KIND_ENV: &str = "GITCOMET_AUTH_KIND";
pub const GITCOMET_AUTH_USERNAME_ENV: &str = "GITCOMET_AUTH_USERNAME";
pub const GITCOMET_AUTH_SECRET_ENV: &str = "GITCOMET_AUTH_SECRET";

pub const GITCOMET_AUTH_KIND_USERNAME_PASSWORD: &str = "username_password";
pub const GITCOMET_AUTH_KIND_PASSPHRASE: &str = "passphrase";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitAuthKind {
    UsernamePassword,
    Passphrase,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedGitAuth {
    pub kind: GitAuthKind,
    pub username: Option<String>,
    pub secret: String,
}

fn staged_git_auth_slot() -> &'static Mutex<Option<StagedGitAuth>> {
    static SLOT: OnceLock<Mutex<Option<StagedGitAuth>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

pub fn clear_staged_git_auth() {
    let slot = staged_git_auth_slot();
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

pub fn stage_git_auth(auth: StagedGitAuth) {
    let slot = staged_git_auth_slot();
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(auth);
}

pub fn take_staged_git_auth() -> Option<StagedGitAuth> {
    let slot = staged_git_auth_slot();
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    guard.take()
}
