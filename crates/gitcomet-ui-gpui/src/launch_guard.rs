use std::any::Any;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiLaunchError {
    context: &'static str,
    panic_message: String,
}

impl UiLaunchError {
    pub fn context(&self) -> &'static str {
        self.context
    }

    pub fn panic_message(&self) -> &str {
        &self.panic_message
    }

    pub(crate) fn from_panic(context: &'static str, payload: Box<dyn Any + Send>) -> Self {
        Self {
            context,
            panic_message: panic_payload_to_string(payload.as_ref()),
        }
    }
}

impl fmt::Display for UiLaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} panicked: {}", self.context, self.panic_message)
    }
}

impl std::error::Error for UiLaunchError {}

pub(crate) fn run_with_panic_guard<F>(context: &'static str, launch: F) -> Result<(), UiLaunchError>
where
    F: FnOnce(),
{
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(launch))
        .map_err(|payload| UiLaunchError::from_panic(context, payload))
}

fn panic_payload_to_string(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_guard_returns_ok_for_successful_launch() {
        assert!(run_with_panic_guard("main window", || {}).is_ok());
    }

    #[test]
    fn panic_guard_captures_string_payload_and_context() {
        let err = run_with_panic_guard("main window", || panic!("wayland init failed"))
            .expect_err("launch should fail");
        assert_eq!(err.context(), "main window");
        assert_eq!(err.panic_message(), "wayland init failed");
        assert_eq!(err.to_string(), "main window panicked: wayland init failed");
    }

    #[test]
    fn panic_guard_handles_non_string_payloads() {
        let err = run_with_panic_guard("main window", || std::panic::panic_any(42_u8))
            .expect_err("launch should fail");
        assert_eq!(err.panic_message(), "<non-string panic payload>");
    }
}
