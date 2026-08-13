use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{application::Clock, sources::pkce::random_urlsafe, GlanceletError, Result};

use super::{DeviceTokenPoll, GitlabApiClient, GitlabCredential, GitlabIdentity, GitlabInstance};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitlabDeviceAuthorization {
    pub session_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub retry_after_seconds: i64,
}

pub enum GitlabDevicePollResult {
    Pending { retry_after_seconds: i64 },
    Authorized(GitlabAuthorization),
}

pub struct GitlabAuthorization {
    pub credential: GitlabCredential,
    pub identity: GitlabIdentity,
    pub instance: GitlabInstance,
}

#[derive(Clone)]
struct PendingSession {
    client_id: String,
    instance: GitlabInstance,
    device_code: String,
    expires_at: DateTime<Utc>,
    interval_seconds: i64,
    next_poll_at: DateTime<Utc>,
}

pub struct GitlabDeviceFlowService {
    client: Arc<GitlabApiClient>,
    clock: Arc<dyn Clock>,
    sessions: std::sync::Mutex<HashMap<String, PendingSession>>,
}

impl GitlabDeviceFlowService {
    pub fn new(client: Arc<GitlabApiClient>, clock: Arc<dyn Clock>) -> Self {
        Self {
            client,
            clock,
            sessions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub async fn begin(
        &self,
        instance: GitlabInstance,
        client_id: &str,
    ) -> Result<GitlabDeviceAuthorization> {
        let response = self
            .client
            .request_device_code(&instance, client_id)
            .await?;
        if response.device_code.trim().is_empty()
            || response.user_code.trim().is_empty()
            || response.verification_uri.trim().is_empty()
            || response.expires_in <= 0
        {
            return Err(GlanceletError::OAuth(
                "GitLab returned an invalid Device Flow challenge".into(),
            ));
        }
        let now = self.clock.now();
        let interval_seconds = response.interval.max(1);
        let expires_at = now + chrono::Duration::seconds(response.expires_in);
        let session_id = random_urlsafe(32);
        let mut sessions = self
            .sessions
            .lock()
            .expect("GitLab Device Flow session map poisoned");
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(
            session_id.clone(),
            PendingSession {
                client_id: client_id.into(),
                instance,
                device_code: response.device_code,
                expires_at,
                interval_seconds,
                next_poll_at: now,
            },
        );
        drop(sessions);
        Ok(GitlabDeviceAuthorization {
            session_id,
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            verification_uri_complete: response.verification_uri_complete,
            expires_at,
            retry_after_seconds: interval_seconds,
        })
    }

    pub async fn poll(&self, session_id: &str) -> Result<GitlabDevicePollResult> {
        let now = self.clock.now();
        let mut session = self
            .sessions
            .lock()
            .expect("GitLab Device Flow session map poisoned")
            .remove(session_id)
            .ok_or_else(|| {
                GlanceletError::OAuth("GitLab Device Flow session is invalid or finished".into())
            })?;
        if session.expires_at <= now {
            return Err(GlanceletError::OAuth(
                "GitLab Device Flow code expired; start the connection again".into(),
            ));
        }
        if session.next_poll_at > now {
            let retry_after_seconds = (session.next_poll_at - now).num_seconds().max(1);
            self.store_session(session_id, session);
            return Ok(GitlabDevicePollResult::Pending {
                retry_after_seconds,
            });
        }
        match self
            .client
            .poll_device_token(&session.instance, &session.client_id, &session.device_code)
            .await
        {
            Ok(DeviceTokenPoll::Authorized(credential)) => {
                let identity = self
                    .client
                    .authenticated_user(&session.instance, &credential.auth())
                    .await?;
                Ok(GitlabDevicePollResult::Authorized(GitlabAuthorization {
                    credential,
                    identity,
                    instance: session.instance,
                }))
            }
            Ok(DeviceTokenPoll::Pending) => {
                session.next_poll_at = now + chrono::Duration::seconds(session.interval_seconds);
                let retry_after_seconds = session.interval_seconds;
                self.store_session(session_id, session);
                Ok(GitlabDevicePollResult::Pending {
                    retry_after_seconds,
                })
            }
            Ok(DeviceTokenPoll::SlowDown) => {
                session.interval_seconds += 5;
                session.next_poll_at = now + chrono::Duration::seconds(session.interval_seconds);
                let retry_after_seconds = session.interval_seconds;
                self.store_session(session_id, session);
                Ok(GitlabDevicePollResult::Pending {
                    retry_after_seconds,
                })
            }
            Ok(DeviceTokenPoll::AccessDenied) => Err(GlanceletError::OAuth(
                "GitLab Device Flow authorization was denied".into(),
            )),
            Ok(DeviceTokenPoll::Expired) => Err(GlanceletError::OAuth(
                "GitLab Device Flow code expired; start the connection again".into(),
            )),
            Err(error) => {
                if !matches!(error, GlanceletError::OAuth(_)) {
                    session.next_poll_at =
                        now + chrono::Duration::seconds(session.interval_seconds);
                    self.store_session(session_id, session);
                }
                Err(error)
            }
        }
    }

    pub fn cancel(&self, session_id: &str) {
        self.sessions
            .lock()
            .expect("GitLab Device Flow session map poisoned")
            .remove(session_id);
    }

    fn store_session(&self, session_id: &str, session: PendingSession) {
        self.sessions
            .lock()
            .expect("GitLab Device Flow session map poisoned")
            .insert(session_id.into(), session);
    }
}
