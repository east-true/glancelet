use std::{collections::HashMap, sync::Arc};

use url::Url;

use crate::{
    application::Clock,
    sources::pkce::{random_urlsafe, PkcePair},
    GlanceletError, Result,
};

use super::{GoogleApiClient, GoogleCredential, GoogleIdentity, CALENDAR_SCOPE};

const AUTHORIZE_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const OIDC_SCOPES: [&str; 3] = ["openid", "email", CALENDAR_SCOPE];

pub struct GoogleAuthorizationStart {
    pub authorization_url: String,
    pub state: String,
}

pub struct GoogleAuthorization {
    pub credential: GoogleCredential,
    pub identity: GoogleIdentity,
}

struct PendingSession {
    client_id: String,
    verifier: String,
    redirect_uri: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

pub struct GoogleOAuthService {
    client: Arc<GoogleApiClient>,
    clock: Arc<dyn Clock>,
    authorize_endpoint: String,
    sessions: std::sync::Mutex<HashMap<String, PendingSession>>,
}

impl GoogleOAuthService {
    pub fn production(client: Arc<GoogleApiClient>, clock: Arc<dyn Clock>) -> Self {
        Self::new(client, clock, AUTHORIZE_ENDPOINT)
    }

    pub fn new(
        client: Arc<GoogleApiClient>,
        clock: Arc<dyn Clock>,
        authorize_endpoint: impl Into<String>,
    ) -> Self {
        Self {
            client,
            clock,
            authorize_endpoint: authorize_endpoint.into(),
            sessions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn begin(&self, client_id: &str, redirect_uri: &str) -> Result<GoogleAuthorizationStart> {
        if client_id.trim().is_empty() {
            return Err(GlanceletError::OAuth(
                "Google OAuth client ID is not configured".into(),
            ));
        }
        let state = random_urlsafe(32);
        let pkce = PkcePair::generate();
        let mut url = Url::parse(&self.authorize_endpoint)
            .map_err(|_| GlanceletError::OAuth("invalid Google authorize endpoint".into()))?;
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &OIDC_SCOPES.join(" "))
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", &state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        self.sessions
            .lock()
            .expect("Google OAuth session map poisoned")
            .insert(
                state.clone(),
                PendingSession {
                    client_id: client_id.into(),
                    verifier: pkce.verifier,
                    redirect_uri: redirect_uri.into(),
                    expires_at: self.clock.now() + chrono::Duration::minutes(10),
                },
            );
        Ok(GoogleAuthorizationStart {
            authorization_url: url.to_string(),
            state,
        })
    }

    pub async fn finish(&self, state: &str, code: &str) -> Result<GoogleAuthorization> {
        let session = self
            .sessions
            .lock()
            .expect("Google OAuth session map poisoned")
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
        let identity = self.client.userinfo(credential.access_token()).await?;
        if identity.sub.trim().is_empty() || identity.email.trim().is_empty() {
            return Err(GlanceletError::OAuth(
                "Google UserInfo omitted account identity".into(),
            ));
        }
        Ok(GoogleAuthorization {
            credential,
            identity,
        })
    }

    pub fn cancel(&self, state: &str) {
        self.sessions
            .lock()
            .expect("Google OAuth session map poisoned")
            .remove(state);
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use chrono::{TimeZone, Utc};
    use reqwest::Client;
    use url::Url;

    use crate::{
        application::FixedClock,
        sources::google::{GoogleApiClient, GoogleOAuthService},
    };

    fn service(clock: Arc<FixedClock>) -> GoogleOAuthService {
        GoogleOAuthService::new(
            Arc::new(GoogleApiClient::new(
                Client::new(),
                "http://127.0.0.1:1",
                "http://127.0.0.1:1/token",
                "http://127.0.0.1:1/userinfo",
            )),
            clock,
            "https://accounts.test/authorize",
        )
    }

    #[test]
    fn desktop_authorization_uses_pkce_offline_access_and_readonly_scope() {
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 8, 11, 0, 0, 0).single().unwrap(),
        ));
        let start = service(clock)
            .begin("client", "http://127.0.0.1:49152")
            .unwrap();
        let url = Url::parse(&start.authorization_url).unwrap();
        let values = url.query_pairs().collect::<HashMap<_, _>>();
        assert_eq!(values.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(values.get("access_type").unwrap(), "offline");
        assert!(values.get("scope").unwrap().contains(super::CALENDAR_SCOPE));
        assert_eq!(values.get("state").unwrap(), &start.state);
    }

    #[tokio::test]
    async fn state_expiry_and_replay_are_rejected_before_exchange() {
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 8, 11, 0, 0, 0).single().unwrap(),
        ));
        let service = service(clock.clone());
        let start = service.begin("client", "http://127.0.0.1:49152").unwrap();
        assert!(service.finish("wrong", "code").await.is_err());
        clock.set(
            Utc.with_ymd_and_hms(2026, 8, 11, 0, 11, 0)
                .single()
                .unwrap(),
        );
        assert!(service.finish(&start.state, "code").await.is_err());
        assert!(service.finish(&start.state, "code").await.is_err());
    }
}
