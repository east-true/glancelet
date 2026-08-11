use std::{collections::HashMap, sync::Arc};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    application::Clock,
    sources::slack::{SlackApiClient, SlackCredential, SlackIdentity},
    GlanceletError, Result,
};

pub struct SlackAuthorizationStart {
    pub authorization_url: String,
    pub state: String,
}

pub struct SlackAuthorization {
    pub credential: SlackCredential,
    pub identity: SlackIdentity,
}

struct PendingSession {
    client_id: String,
    verifier: String,
    redirect_uri: String,
    expires_at: DateTime<Utc>,
}

pub struct SlackOAuthService {
    client: Arc<SlackApiClient>,
    clock: Arc<dyn Clock>,
    sessions: std::sync::Mutex<HashMap<String, PendingSession>>,
    authorize_base: String,
}

impl SlackOAuthService {
    pub fn production(client: Arc<SlackApiClient>, clock: Arc<dyn Clock>) -> Self {
        Self::new(client, clock, "https://slack.com/oauth/v2_user/authorize")
    }

    pub fn new(
        client: Arc<SlackApiClient>,
        clock: Arc<dyn Clock>,
        authorize_base: impl Into<String>,
    ) -> Self {
        Self {
            client,
            clock,
            sessions: std::sync::Mutex::new(HashMap::new()),
            authorize_base: authorize_base.into(),
        }
    }

    pub fn begin(&self, client_id: &str, redirect_uri: &str) -> Result<SlackAuthorizationStart> {
        if client_id.trim().is_empty() {
            return Err(GlanceletError::OAuth(
                "GLANCELET_SLACK_CLIENT_ID is not configured".into(),
            ));
        }
        let state = random_urlsafe(32);
        let verifier = random_urlsafe(64);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse(&self.authorize_base)
            .map_err(|_| GlanceletError::OAuth("invalid Slack authorize endpoint".into()))?;
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("scope", "reactions:read")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");

        self.sessions
            .lock()
            .expect("Slack OAuth session map poisoned")
            .insert(
                state.clone(),
                PendingSession {
                    client_id: client_id.into(),
                    verifier,
                    redirect_uri: redirect_uri.into(),
                    expires_at: self.clock.now() + chrono::Duration::minutes(10),
                },
            );
        Ok(SlackAuthorizationStart {
            authorization_url: url.to_string(),
            state,
        })
    }

    pub async fn finish(&self, state: &str, code: &str) -> Result<SlackAuthorization> {
        let session = self
            .sessions
            .lock()
            .expect("Slack OAuth session map poisoned")
            .remove(state)
            .ok_or_else(|| {
                GlanceletError::OAuth("OAuth state is invalid or already used".into())
            })?;
        if session.expires_at <= self.clock.now() {
            return Err(GlanceletError::OAuth("OAuth session expired".into()));
        }
        if code.is_empty() {
            return Err(GlanceletError::OAuth(
                "OAuth callback omitted the code".into(),
            ));
        }
        let credential = self
            .client
            .exchange_code(
                &session.client_id,
                code,
                &session.verifier,
                &session.redirect_uri,
                self.clock.now(),
            )
            .await?;
        let identity = self.client.auth_test(credential.access_token()).await?;
        Ok(SlackAuthorization {
            credential,
            identity,
        })
    }

    pub fn cancel(&self, state: &str) {
        self.sessions
            .lock()
            .expect("Slack OAuth session map poisoned")
            .remove(state);
    }
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use reqwest::Client;
    use url::Url;

    use crate::{
        application::FixedClock,
        sources::slack::{SlackApiClient, SlackOAuthService},
    };

    #[test]
    fn pkce_uses_s256_and_random_state() {
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 8, 11, 0, 0, 0).single().unwrap(),
        ));
        let client = Arc::new(SlackApiClient::new(Client::new(), "http://127.0.0.1:1"));
        let service = SlackOAuthService::new(client, clock, "https://slack.test/authorize");
        let start = service
            .begin("client", "http://localhost:42813/oauth/slack/callback")
            .unwrap();
        let url = Url::parse(&start.authorization_url).unwrap();
        let values = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(values.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(values.get("scope").unwrap(), "reactions:read");
        assert!(values.get("code_challenge").unwrap().len() >= 43);
        assert_eq!(values.get("state").unwrap(), &start.state);
    }

    #[tokio::test]
    async fn state_mismatch_and_expired_sessions_are_rejected_before_exchange() {
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 8, 11, 0, 0, 0).single().unwrap(),
        ));
        let client = Arc::new(SlackApiClient::new(Client::new(), "http://127.0.0.1:1"));
        let service = SlackOAuthService::new(client, clock.clone(), "https://slack.test/authorize");
        let start = service
            .begin("client", "http://localhost/callback")
            .unwrap();
        assert!(service.finish("wrong-state", "code").await.is_err());
        clock.set(
            Utc.with_ymd_and_hms(2026, 8, 11, 0, 11, 0)
                .single()
                .unwrap(),
        );
        assert!(service.finish(&start.state, "code").await.is_err());
        assert!(service.finish(&start.state, "code").await.is_err());
    }
}
