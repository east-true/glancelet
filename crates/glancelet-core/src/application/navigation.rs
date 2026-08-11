use std::sync::Arc;

use serde_json::Value;
use url::Url;

use crate::{application::WorkStore, GlanceletError, Result};

pub struct NavigationService {
    store: Arc<dyn WorkStore>,
}

impl NavigationService {
    pub fn new(store: Arc<dyn WorkStore>) -> Self {
        Self { store }
    }

    /// Returns a validated target for the platform-specific opener. It never
    /// accepts commands, file paths, or credentials embedded in a URL.
    pub fn open_source_target(&self, work_id: &str) -> Result<String> {
        let work = self.store.stored_work_by_id(work_id)?;
        validated_target(&work.navigation)
    }
}

pub(super) fn validated_target(value: &Value) -> Result<String> {
    let mut last_error = None;
    for raw in ["app_url", "web_url"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
    {
        match validate_target(raw) {
            Ok(target) => return Ok(target),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| GlanceletError::InvalidNavigation("target is missing".into())))
}

fn validate_target(raw: &str) -> Result<String> {
    let url =
        Url::parse(raw).map_err(|error| GlanceletError::InvalidNavigation(error.to_string()))?;
    const ALLOWED_SCHEMES: &[&str] = &["https", "http", "slack", "notion", "msteams"];
    if !ALLOWED_SCHEMES.contains(&url.scheme()) {
        return Err(GlanceletError::InvalidNavigation(format!(
            "scheme '{}' is not allowed",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GlanceletError::InvalidNavigation(
            "credentials are not allowed in navigation targets".into(),
        ));
    }
    if matches!(url.scheme(), "http" | "https") && url.host_str().is_none() {
        return Err(GlanceletError::InvalidNavigation(
            "web navigation target must include a host".into(),
        ));
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_target;

    #[test]
    fn only_safe_navigation_schemes_are_allowed() {
        assert!(validate_target("https://example.test/task/1").is_ok());
        assert!(validate_target("slack://channel?id=C1").is_ok());
        assert!(validate_target("file:///etc/passwd").is_err());
        assert!(validate_target("javascript:alert(1)").is_err());
        assert!(validate_target("https://user:secret@example.test").is_err());
    }
}
