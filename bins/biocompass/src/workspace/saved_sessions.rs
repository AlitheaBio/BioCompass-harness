//! Purpose-limited browser access to files already in this installation.
use super::*;
use biocompass_core_traits::local_delivery::SavedReadSession;
use chrono::{DateTime, Utc};
use std::error::Error as _;
use std::io::ErrorKind;

const HANDLE_HEADER: &str = "x-workspace-read";

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn handle_for(bearer: &str, workspace: &str) -> String {
    digest(&format!("bc-local-saved-read-v1:{workspace}:{bearer}"))
}

fn status_fingerprint(bearer: &str, workspace: &str) -> String {
    digest(&format!(
        "bc-workspace-read-v1:{}:{workspace}",
        digest(bearer)
    ))
}

fn supplied_handle(headers: &HeaderMap) -> CompassResult<Option<&str>> {
    let Some(header) = headers.get(HANDLE_HEADER) else {
        return Ok(None);
    };
    header
        .to_str()
        .ok()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .map(Some)
        .ok_or_else(reauth)
}

fn reauth() -> CompassError {
    CompassError::AccessDenied("UNAUTHENTICATED: Sign in again to open saved results.".into())
}

fn bearer(auth: &CallerAuth) -> Option<&str> {
    auth.authorization
        .as_deref()?
        .strip_prefix("Bearer ")
        .filter(|v| !v.is_empty())
}

fn offline(error: &reqwest::Error) -> bool {
    if error.is_timeout() {
        return true;
    }
    let mut source = error.source();
    while let Some(cause) = source {
        // hyper-util's connector marks DNS resolution separately from TLS errors.
        // The OS resolver can report an unclassified io::Error for a disconnected DNS server.
        if error.is_connect() && cause.to_string() == "dns error" {
            return true;
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::NetworkUnreachable
                    | ErrorKind::HostUnreachable
                    | ErrorKind::NotConnected
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::TimedOut
                    | ErrorKind::BrokenPipe
            ) {
                return true;
            }
        source = cause.source();
    }
    false
}

pub(super) struct ReadAccess {
    pub scope: DeliveryScope,
    pub expires_at: Option<DateTime<Utc>>,
    pub offline: bool,
}

#[derive(Deserialize)]
struct ReadStatus {
    session_expires_at: DateTime<Utc>,
}

impl InstalledWorkspace {
    // Pairing, account metadata and identity must all refer to exactly this bearer.
    pub(super) fn browser_request(&self, method: Method, path: &[&str]) -> reqwest::RequestBuilder {
        if let Some(secret) = bearer(&current_caller_auth()) {
            self.unsigned_request(method, path).bearer_auth(secret)
        } else {
            self.request(method, path)
        }
    }

    pub(super) async fn remember_browser_session(&self, profile: &mut Value) -> CompassResult<()> {
        let auth = current_caller_auth();
        let Some(token) = bearer(&auth) else {
            return Ok(());
        };
        if !self.matches_profile(profile) {
            self.store
                .revoke_read_session(digest(&handle_for(token, self.store.workspace_id())))
                .await?;
            return Ok(());
        }
        let principal: Principal = self
            .json(
                self.unsigned_request(Method::GET, &["auth", "whoami"])
                    .bearer_auth(token),
            )
            .await?;
        if principal.auth_method != "session" {
            return Ok(());
        }
        let (Some(user), Some(expires_at)) = (principal.user_id, principal.session_expires_at)
        else {
            // Older hubs can still serve an authenticated result while they are reachable.
            return Ok(());
        };
        if expires_at <= Utc::now() || user.is_empty() {
            return Err(reauth());
        }
        let workspace = self.store.workspace_id();
        let handle = handle_for(token, workspace);
        let entry = SavedReadSession {
            scope: DeliveryScope {
                origin: self.hub_url().to_owned(),
                org_id: principal.org_id,
                subject: format!("user:{user}"),
            },
            fingerprint: status_fingerprint(token, workspace),
            expires_at,
        };
        self.store
            .remember_read_session(digest(&handle), entry)
            .await?;
        profile["read_session_token"] = json!(handle);
        profile["read_session_expires_at"] = json!(expires_at);
        Ok(())
    }

    pub(super) async fn forget_browser_session(
        &self,
        headers: &HeaderMap,
        include_bearer: bool,
    ) -> CompassResult<()> {
        let handle = supplied_handle(headers)?;
        if let Some(handle) = handle {
            self.store.revoke_read_session(digest(handle)).await?;
        }
        let auth = current_caller_auth();
        let by_bearer = include_bearer
            .then(|| bearer(&auth))
            .flatten()
            .map(|token| handle_for(token, self.store.workspace_id()));
        if let Some(handle) = &by_bearer {
            self.store.revoke_read_session(digest(handle)).await?;
        }
        let mut sync = self.sync_session.lock().await;
        if let Some(token) = sync.as_deref().and_then(|v| v.strip_prefix("Bearer ")) {
            let synced = handle_for(token, self.store.workspace_id());
            if handle == Some(synced.as_str()) || by_bearer.as_ref() == Some(&synced) {
                *sync = None;
            }
        }
        Ok(())
    }

    pub(super) async fn saved_read_access(&self, headers: &HeaderMap) -> CompassResult<ReadAccess> {
        let auth = current_caller_auth();
        let header = supplied_handle(headers)?;
        let by_bearer = bearer(&auth).map(|token| handle_for(token, self.store.workspace_id()));
        if let (Some(claimed), Some(actual)) = (header, by_bearer.as_deref())
            && claimed != actual
        {
            return Err(reauth());
        }
        if header.is_some()
            && (auth.api_key.is_some()
                || auth
                    .cookie
                    .as_ref()
                    .is_some_and(|c| c.split(';').any(|v| v.trim().starts_with("bc_session="))))
        {
            return Err(reauth());
        }
        if let Some(handle) = header.or(by_bearer.as_deref()) {
            let hash = digest(handle);
            if let Some(entry) = self
                .store
                .read_session(hash.clone(), self.hub_url().to_owned())
                .await?
            {
                return self.check_saved_session(hash, entry).await;
            }
            if header.is_some() {
                return Err(reauth());
            }
        }
        // Existing online API and CLI clients may use their normal hub credentials.
        if auth.authorization.is_none() && auth.api_key.is_none() && auth.cookie.is_none() {
            return Err(reauth());
        }
        Ok(ReadAccess {
            scope: self.scope().await?,
            expires_at: None,
            offline: false,
        })
    }

    async fn check_saved_session(
        &self,
        hash: String,
        entry: SavedReadSession,
    ) -> CompassResult<ReadAccess> {
        let reply = self.unsigned_request(Method::POST, &["auth","workspace-read-session"])
            .timeout(Duration::from_secs(4))
            .json(&json!({"workspace_id": self.store.workspace_id(),"session_fingerprint":entry.fingerprint}))
            .send().await;
        let disconnected = match reply {
            Err(error) if offline(&error) => true,
            Err(_) => {
                return Err(CompassError::Storage(
                    "Could not verify the home hub connection.".into(),
                ));
            }
            Ok(reply)
                if matches!(
                    reply.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                ) =>
            {
                self.store.revoke_read_session(hash).await?;
                return Err(map_http_error(reply).await);
            }
            Ok(reply) if !reply.status().is_success() => return Err(map_http_error(reply).await),
            Ok(reply) => {
                let status: ReadStatus = reply.json().await.map_err(|_| {
                    CompassError::Protocol("Invalid home hub session status.".into())
                })?;
                if status.session_expires_at <= Utc::now() {
                    self.store.revoke_read_session(hash).await?;
                    return Err(reauth());
                }
                if status.session_expires_at < entry.expires_at {
                    self.store
                        .shorten_read_session(hash.clone(), status.session_expires_at)
                        .await?;
                }
                false
            }
        };
        // Sign-out or expiry may have happened while waiting on the network.
        let current = self
            .store
            .read_session(hash, self.hub_url().to_owned())
            .await?
            .ok_or_else(reauth)?;
        Ok(ReadAccess {
            scope: current.scope,
            expires_at: Some(current.expires_at),
            offline: disconnected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_handle_and_liveness_fingerprint_have_distinct_purposes() {
        let token = "bcs_offline_test_vector";
        let workspace = "01234567-89ab-4cde-8123-456789abcdef";
        let fingerprint = status_fingerprint(token, workspace);
        assert_eq!(
            fingerprint,
            "a8e7729cdc461a504db6abff014200a6769c1bbb693ee89fbfa03796826e9559"
        );
        assert_ne!(fingerprint, digest(token));
        assert_ne!(fingerprint, handle_for(token, workspace));
        assert_ne!(handle_for(token, workspace), handle_for(token, "other"));
    }
}
