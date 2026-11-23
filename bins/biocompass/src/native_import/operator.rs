//! A terminal launch ticket proves control of this personal installation.
//! Keep its short-lived browser grant in one origin/tab, independent of hub sessions.
use crate::server::ApiResult;
use crate::state::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use biocompass_core_traits::{CompassError, CompassResult};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use std::{
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const HEADER: &str = "x-biocompass-local-owner";
const LINK_TTL: Duration = Duration::from_secs(10 * 60);
const TAB_TTL: Duration = Duration::from_secs(8 * 60 * 60);

struct Grant {
    hash: [u8; 32],
    expires: Instant,
}

impl Grant {
    fn mint(ttl: Duration) -> (Self, String) {
        let raw = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        (
            Self {
                hash: Sha256::digest(raw.as_bytes()).into(),
                expires: Instant::now() + ttl,
            },
            raw,
        )
    }

    fn matches(&self, supplied: &str) -> bool {
        supplied.len() == 64
            && bool::from(Sha256::digest(supplied.as_bytes()).as_slice().ct_eq(&self.hash))
    }
}

struct Sessions {
    ticket: Grant,
    tab: Option<Grant>,
}

pub struct OperatorAccess {
    authority: String,
    private_dir: PathBuf,
    sessions: Mutex<Sessions>,
}

fn locked() -> CompassError {
    CompassError::AccessDenied(
        "UNAUTHENTICATED: open the local owner link shown by biocompass up".into(),
    )
}
fn denied() -> CompassError {
    CompassError::AccessDenied("local owner controls require the personal workbench origin".into())
}

impl OperatorAccess {
    pub fn launch(addr: SocketAddr, dir: &Path) -> CompassResult<(Arc<Self>, String)> {
        if !addr.ip().is_loopback() {
            return Err(denied());
        }
        let authority = addr.to_string();
        let (ticket, link) = Self::new_link(&authority, dir)?;
        Ok((
            Arc::new(Self {
                authority,
                private_dir: dir.to_owned(),
                sessions: Mutex::new(Sessions { ticket, tab: None }),
            }),
            link,
        ))
    }

    fn new_link(authority: &str, dir: &Path) -> CompassResult<(Grant, String)> {
        let (grant, raw) = Grant::mint(LINK_TTL);
        let link = format!("http://{authority}/#local-owner={raw}");
        let mut f = tempfile::NamedTempFile::new_in(dir).map_err(super::storage)?;
        writeln!(f, "{link}").map_err(super::storage)?;
        f.as_file().sync_all().map_err(super::storage)?;
        f.persist(dir.join("owner-link.txt"))
            .map_err(super::storage)?;
        Ok((grant, link))
    }

    fn exchange(&self, supplied: &str) -> CompassResult<String> {
        let mut sessions = self.sessions.lock().map_err(|_| locked())?;
        if !sessions.ticket.matches(supplied) {
            return Err(locked());
        }
        let expired = Instant::now() >= sessions.ticket.expires;
        // Only a holder of the current ticket can rotate the private file. Neither
        // successful nor expired redeemers receive the next launch URL over HTTP.
        sessions.ticket = Self::new_link(&self.authority, &self.private_dir)?.0;
        if expired {
            return Err(CompassError::AccessDenied("UNAUTHENTICATED: owner link expired; open the latest owner-link.txt on this laptop".into()));
        }
        let (grant, raw) = Grant::mint(TAB_TTL);
        sessions.tab = Some(grant);
        Ok(raw)
    }

    fn is_allowed_loopback_host(&self, host: &str) -> bool {
        if host == self.authority {
            return true;
        }
        if let Some((_, port)) = self.authority.rsplit_once(':') {
            return host == format!("localhost:{port}")
                || host == format!("127.0.0.1:{port}")
                || host == format!("[::1]:{port}");
        }
        false
    }

    fn is_allowed_loopback_origin(&self, origin: &str) -> bool {
        let Some(rest) = origin.strip_prefix("http://") else {
            return false;
        };
        if self.is_allowed_loopback_host(rest) {
            return true;
        }
        // In dev mode, Vite serves on loopback :5173/:5174 and proxies API calls.
        let env_dev_cors = std::env::var("BIOCOMPASS_DEV_CORS")
            .ok()
            .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
        if env_dev_cors || cfg!(debug_assertions) {
            for dev_port in [5173_u16, 5174_u16] {
                if rest == format!("localhost:{dev_port}")
                    || rest == format!("127.0.0.1:{dev_port}")
                    || rest == format!("[::1]:{dev_port}")
                {
                    return true;
                }
            }
        }
        false
    }

    fn check_origin(&self, headers: &HeaderMap) -> CompassResult<()> {
        let host_ok = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| self.is_allowed_loopback_host(h));
        if !host_ok {
            return Err(denied());
        }
        if let Some(origin) = headers.get(header::ORIGIN)
            && !origin
                .to_str()
                .ok()
                .is_some_and(|o| self.is_allowed_loopback_origin(o))
        {
            return Err(denied());
        }
        if headers
            .get("sec-fetch-site")
            .is_some_and(|v| v != "same-origin" && v != "none")
        {
            return Err(denied());
        }
        Ok(())
    }

    pub(crate) fn check(&self, headers: &HeaderMap) -> CompassResult<()> {
        self.check_origin(headers)?;
        let token = headers
            .get(HEADER)
            .and_then(|h| h.to_str().ok())
            .ok_or_else(locked)?;
        let sessions = self.sessions.lock().map_err(|_| locked())?;
        if sessions
            .tab
            .as_ref()
            .is_some_and(|g| Instant::now() < g.expires && g.matches(token))
        {
            Ok(())
        } else {
            Err(locked())
        }
    }
}

pub(crate) fn authorize(state: &AppState, headers: &HeaderMap) -> CompassResult<()> {
    state
        .local_owner
        .as_ref()
        .ok_or_else(denied)?
        .check(headers)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Redemption {
    ticket: String,
}

pub(crate) async fn redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Redemption>,
) -> ApiResult<Response> {
    let access = state.local_owner.as_ref().ok_or_else(denied)?;
    access.check_origin(&headers)?;
    let raw = access.exchange(&req.ticket)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"owner_session":raw})),
    )
        .into_response())
}

use axum::extract::Path as AxumPath;

pub(crate) async fn list_changes(
    State(app): State<AppState>,
    AxumPath((ns, name)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    authorize(&app, &headers)?;
    let result = super::changes::list(&app, &ns, &name).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}

pub(crate) async fn preview_change(
    State(app): State<AppState>,
    AxumPath((ns, name)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    authorize(&app, &headers)?;
    let result = super::changes::preview(&app, &ns, &name, body).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}

pub(crate) async fn publish_change(
    State(app): State<AppState>,
    AxumPath((ns, name, id)): AxumPath<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    authorize(&app, &headers)?;
    let result = super::changes::publish(&app, &ns, &name, &id).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_owner_link_refreshes_only_the_private_file_and_preserves_active_tab() {
        let temp = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "127.0.0.1:49123".parse().unwrap();
        let (access, first) = OperatorAccess::launch(addr, temp.path()).unwrap();
        fn token(s: &str) -> &str {
            s.trim().split_once("#local-owner=").unwrap().1
        }
        let first_tab = access.exchange(token(&first)).unwrap();
        let read_link = || std::fs::read_to_string(temp.path().join("owner-link.txt")).unwrap();
        let replaced = read_link();
        assert_ne!(token(&first), token(&replaced));
        assert!(access.exchange(token(&first)).is_err());
        assert_eq!(read_link(), replaced);
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "127.0.0.1:49123".parse().unwrap());
        h.insert(HEADER, first_tab.parse().unwrap());
        access.check(&h).unwrap();
        access.sessions.lock().unwrap().ticket.expires = Instant::now();
        assert!(access.exchange(token(&replaced)).is_err());
        let latest = read_link();
        assert_ne!(latest, replaced);
        access.check(&h).unwrap();
        let second_tab = access.exchange(token(&latest)).unwrap();
        assert!(access.check(&h).is_err());
        h.insert(HEADER, second_tab.parse().unwrap());
        access.check(&h).unwrap();
        access
            .sessions
            .lock()
            .unwrap()
            .tab
            .as_mut()
            .unwrap()
            .expires = Instant::now();
        assert!(access.check(&h).is_err());
    }

    #[test]
    fn r17_operator_grant_uses_constant_time_digest_eq() {
        let src = include_str!("operator.rs");
        let prod_src = src.split("#[cfg(test)]").next().unwrap_or("");
        assert!(
            prod_src.contains("ConstantTimeEq") && prod_src.contains(".ct_eq("),
            "Grant::matches must use subtle::ConstantTimeEq"
        );
        let (grant, raw) = Grant::mint(Duration::from_secs(60));
        assert!(grant.matches(&raw));
        let mut flipped = raw.clone();
        let last = if flipped.ends_with('0') { '1' } else { '0' };
        flipped.pop();
        flipped.push(last);
        assert!(!grant.matches(&flipped));
        assert!(!grant.matches("short"));
    }
}
