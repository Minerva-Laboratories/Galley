//! Sign in with an OpenID Connect provider, such as a university's single sign-on. The flow is the
//! authorization code flow with PKCE and a nonce. The provider's ID token is verified against its
//! published keys, issuer, audience, expiry and the nonce before anyone is signed in.
//!
//! The provider is discovered on first use, not at start, so a provider that is down never stops
//! Galley from starting, and password sign-in keeps working.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, HttpRequest, HttpResponse, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use tokio::sync::RwLock;

use crate::config::OidcConfig;

/// How long a person may take at the provider's sign-in page.
const PENDING_FOR: Duration = Duration::from_secs(600);
/// Sign-ins in progress at once. The start route needs no account, so its memory is bounded.
const MAX_PENDING: usize = 10_000;

/// Who the provider says signed in, after the token checked out.
#[derive(Debug, Clone)]
pub struct Identity {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

struct Pending {
    verifier: PkceCodeVerifier,
    nonce: Nonce,
    started: Instant,
}

pub struct Oidc {
    issuer: IssuerUrl,
    client_id: ClientId,
    client_secret: Option<ClientSecret>,
    redirect: RedirectUrl,
    pub label: String,
    allowed_domains: Vec<String>,
    http: reqwest::Client,
    metadata: RwLock<Option<CoreProviderMetadata>>,
    pending: Mutex<HashMap<String, Pending>>,
}

#[derive(Debug)]
pub struct HttpError(String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

/// Errors carry a message a person can act on. The detail goes to the log.
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("The sign-in provider could not be reached. Try again, or sign in with your password.")]
    Unreachable(String),
    #[error("This sign-in link expired or was already used. Start again.")]
    UnknownState,
    #[error("The sign-in provider did not confirm who you are. Start again.")]
    Rejected(String),
    #[error("{0}")]
    NotAllowed(String),
}

impl Oidc {
    /// `None` when single sign-on is not configured.
    pub fn from_config(config: &OidcConfig, public_url: &str) -> Result<Option<Oidc>, String> {
        if config.issuer.trim().is_empty() {
            return Ok(None);
        }
        if config.client_id.trim().is_empty() {
            return Err("[oidc] issuer is set but client_id is not".into());
        }
        let base = if config.redirect_url.is_empty() { public_url.trim_end_matches('/').to_string() } else { String::new() };
        if config.redirect_url.is_empty() && base.is_empty() {
            return Err("[oidc] needs server.domain, or redirect_url set to https://<your host>/api/auth/oidc/callback".into());
        }
        let redirect = if config.redirect_url.is_empty() {
            format!("{base}/api/auth/oidc/callback")
        } else {
            config.redirect_url.clone()
        };
        let http = reqwest::Client::builder()
            // The token endpoint must answer itself. Following a redirect could send the code elsewhere.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Some(Oidc {
            issuer: IssuerUrl::new(config.issuer.trim().to_string()).map_err(|e| format!("[oidc] issuer: {e}"))?,
            client_id: ClientId::new(config.client_id.trim().to_string()),
            client_secret: config.client_secret.resolve("GALLEY_OIDC_CLIENT_SECRET").map(ClientSecret::new),
            redirect: RedirectUrl::new(redirect).map_err(|e| format!("[oidc] redirect_url: {e}"))?,
            label: if config.label.trim().is_empty() { "Continue with single sign-on".into() } else { config.label.trim().to_string() },
            allowed_domains: config.allowed_domains.iter().map(|d| d.trim().trim_start_matches('@').to_lowercase()).filter(|d| !d.is_empty()).collect(),
            http,
            metadata: RwLock::new(None),
            pending: Mutex::new(HashMap::new()),
        }))
    }

    fn http_client(&self) -> impl Fn(HttpRequest) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpResponse, HttpError>> + Send>> + '_ {
        move |request: HttpRequest| {
            let client = self.http.clone();
            Box::pin(async move {
                let (parts, body) = request.into_parts();
                let mut req = client.request(parts.method, parts.uri.to_string()).body(body);
                for (name, value) in &parts.headers {
                    req = req.header(name, value);
                }
                let response = req.send().await.map_err(|e| HttpError(e.without_url().to_string()))?;
                let mut builder = axum::http::Response::builder().status(response.status());
                for (name, value) in response.headers() {
                    builder = builder.header(name, value);
                }
                let bytes = response.bytes().await.map_err(|e| HttpError(e.to_string()))?;
                builder.body(bytes.to_vec()).map_err(|e| HttpError(e.to_string()))
            })
        }
    }

    async fn metadata(&self) -> Result<CoreProviderMetadata, OidcError> {
        if let Some(m) = self.metadata.read().await.as_ref() {
            return Ok(m.clone());
        }
        let found = CoreProviderMetadata::discover_async(self.issuer.clone(), &self.http_client())
            .await
            .map_err(|e| OidcError::Unreachable(format!("discovery: {e}")))?;
        *self.metadata.write().await = Some(found.clone());
        Ok(found)
    }

    /// The provider URL to send the person to, and the state that must come back with them.
    pub async fn start(&self) -> Result<(String, String), OidcError> {
        let meta = self.metadata().await?;
        let client = CoreClient::from_provider_metadata(meta, self.client_id.clone(), self.client_secret.clone())
            .set_redirect_uri(self.redirect.clone());
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random)
            .add_scope(Scope::new("email".into()))
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(challenge)
            .url();
        let mut pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain(|_, p| p.started.elapsed() < PENDING_FOR);
        if pending.len() >= MAX_PENDING {
            return Err(OidcError::Unreachable("too many sign-ins in progress".into()));
        }
        pending.insert(state.secret().clone(), Pending { verifier, nonce, started: Instant::now() });
        Ok((url.to_string(), state.secret().clone()))
    }

    /// Finish a sign-in: exchange the code, verify the ID token and check the allowed domains.
    pub async fn finish(&self, code: &str, state: &str) -> Result<Identity, OidcError> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(state)
            .filter(|p| p.started.elapsed() < PENDING_FOR)
            .ok_or(OidcError::UnknownState)?;
        let meta = self.metadata().await?;
        let client = CoreClient::from_provider_metadata(meta, self.client_id.clone(), self.client_secret.clone())
            .set_redirect_uri(self.redirect.clone());
        let http = self.http_client();
        let tokens = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|e| OidcError::Rejected(format!("token endpoint: {e}")))?
            .set_pkce_verifier(pending.verifier)
            .request_async(&http)
            .await
            .map_err(|e| OidcError::Rejected(format!("token exchange: {e}")))?;
        let id_token = tokens.id_token().ok_or_else(|| OidcError::Rejected("no ID token".into()))?;
        let claims = id_token
            .claims(&client.id_token_verifier(), &pending.nonce)
            .map_err(|e| OidcError::Rejected(format!("ID token: {e}")))?;
        let identity = Identity {
            subject: claims.subject().to_string(),
            email: claims.email().map(|e| e.to_string().trim().to_lowercase()),
            email_verified: claims.email_verified().unwrap_or(false),
            name: claims.name().and_then(|n| n.get(None)).map(|n| n.to_string()),
        };
        self.check_domain(&identity)?;
        Ok(identity)
    }

    /// Whether only listed email domains get in. Then the list, not public signup, decides who
    /// may create an account.
    pub fn restricts_domains(&self) -> bool {
        !self.allowed_domains.is_empty()
    }

    fn check_domain(&self, identity: &Identity) -> Result<(), OidcError> {
        if self.allowed_domains.is_empty() {
            return Ok(());
        }
        let domain = identity.email.as_deref().and_then(|e| e.rsplit_once('@')).map(|(_, d)| d);
        match domain {
            Some(d) if identity.email_verified && self.allowed_domains.iter().any(|a| a == d) => Ok(()),
            _ => Err(OidcError::NotAllowed(format!(
                "This server accepts accounts from {} only. Ask an admin for an invitation.",
                self.allowed_domains.join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oidc(domains: &[&str]) -> Oidc {
        let config = OidcConfig {
            issuer: "https://login.example.edu".into(),
            client_id: "galley".into(),
            allowed_domains: domains.iter().map(|d| d.to_string()).collect(),
            ..OidcConfig::default()
        };
        Oidc::from_config(&config, "https://galley.example.edu").unwrap().unwrap()
    }

    fn who(email: &str, verified: bool) -> Identity {
        Identity { subject: "s".into(), email: Some(email.into()), email_verified: verified, name: None }
    }

    #[test]
    fn allowed_domains_need_a_verified_email_in_them() {
        let o = oidc(&["@example.edu"]);
        assert!(o.check_domain(&who("alicia@example.edu", true)).is_ok());
        assert!(o.check_domain(&who("alicia@example.edu", false)).is_err(), "an unverified email proves nothing");
        assert!(o.check_domain(&who("bob@elsewhere.org", true)).is_err());
        assert!(o.check_domain(&who("bob@sub.example.edu", true)).is_err(), "only the listed domain");
        assert!(oidc(&[]).check_domain(&who("bob@elsewhere.org", false)).is_ok(), "no list, no restriction");
    }

    #[test]
    fn the_callback_comes_from_the_server_domain() {
        let o = oidc(&[]);
        assert_eq!(o.redirect.as_str(), "https://galley.example.edu/api/auth/oidc/callback");
        let none = OidcConfig { issuer: "https://login.example.edu".into(), client_id: "g".into(), ..OidcConfig::default() };
        assert!(Oidc::from_config(&none, "").is_err(), "no domain and no redirect_url is a setup error");
        assert!(Oidc::from_config(&OidcConfig::default(), "").unwrap().is_none(), "off by default");
    }

    #[tokio::test]
    async fn a_state_works_once() {
        let o = oidc(&[]);
        o.pending.lock().unwrap().insert(
            "st".into(),
            Pending { verifier: PkceCodeVerifier::new("v".repeat(43)), nonce: Nonce::new("n".into()), started: Instant::now() },
        );
        // The provider is unreachable here, so the first use fails after it consumed the state.
        assert!(matches!(o.finish("code", "st").await, Err(OidcError::Unreachable(_))));
        assert!(matches!(o.finish("code", "st").await, Err(OidcError::UnknownState)));
        assert!(matches!(o.finish("code", "never-issued").await, Err(OidcError::UnknownState)));
    }
}
