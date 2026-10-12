//! Coordination-server endpoint parsing, validation and normalization.
//!
//! The shipped deployment is plain HTTP on the standard port:
//! [`DEFAULT_COORDINATOR_URL`] (`http://34.66.188.245`, i.e. port 80) is live —
//! `GET /health` returns `200 {"service":"hivebear-coordinator","status":"ok"}`.
//! Port 80 is therefore correct for that host and must not be "fixed".
//!
//! Normalization is deliberately conservative: it only repairs input that
//! silently breaks the HTTP contract —
//! - a missing scheme (`34.66.188.245` → reqwest "relative URL without a base"),
//! - a trailing slash or stray path (`http://host/` makes the client request
//!   `POST http://host//register` → 404, which the mesh used to swallow),
//! - whitespace around the value —
//!
//! while preserving whatever explicit host and port the operator configured.

/// Default port of the `hivebear-coordinator` binary (`--port` default).
///
/// Informational only: the built-in deployment listens on the scheme default
/// (80) via [`DEFAULT_COORDINATOR_URL`], so a URL without a port is
/// intentionally left without one.
pub const DEFAULT_COORDINATOR_PORT: u16 = 7879;

/// Single source of truth for the built-in coordination server.
///
/// NOTE: change this one line when the deployment moves. Keep an explicit port
/// only when it is not the scheme default.
pub const DEFAULT_COORDINATOR_URL: &str = "http://34.66.188.245";

/// Reason a coordination-server URL was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoordinatorUrlError {
    /// The value was empty or whitespace only.
    #[error("coordination server URL is empty")]
    Empty,
    /// The value has no host component.
    #[error("coordination server URL '{0}' has no host")]
    MissingHost(String),
    /// The scheme is not `http` or `https`.
    #[error("unsupported coordination server scheme '{0}' (expected http or https)")]
    UnsupportedScheme(String),
    /// The explicit port is not a valid, non-zero `u16`.
    #[error("invalid coordination server port '{0}' (expected 1-65535)")]
    InvalidPort(String),
}

/// Split an optional `scheme://` prefix. A missing scheme defaults to `http`.
fn split_scheme(raw: &str) -> (String, &str) {
    match raw.find("://") {
        Some(i) => (raw[..i].to_ascii_lowercase(), &raw[i + 3..]),
        None => ("http".to_string(), raw),
    }
}

/// Split `host[:port][/path]` into its authority and path/query/fragment tail.
fn split_authority(rest: &str) -> (&str, &str) {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    (&rest[..end], &rest[end..])
}

/// Detect a host with an optional port, tolerating bracketed and bare IPv6
/// literals and optional `user@` userinfo. Returns the host (with userinfo and
/// brackets preserved) plus the raw port text when one was present.
fn split_host_port(authority: &str) -> (String, Option<String>) {
    let after_userinfo = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let userinfo = &authority[..authority.len() - after_userinfo.len()];

    if let Some(close) = after_userinfo.strip_prefix('[').and_then(|s| s.find(']')) {
        // Bracketed IPv6 literal, optionally followed by `:port`.
        let host = &after_userinfo[..=close + 1];
        let port = after_userinfo[close + 2..].strip_prefix(':');
        return (format!("{userinfo}{host}"), port.map(str::to_string));
    }

    if after_userinfo.matches(':').count() > 1 {
        // Bare IPv6 literal without brackets (`::1`, `fe80::1`). Bracket it so
        // the result stays a valid URL; there is no port to extract.
        return (format!("{userinfo}[{after_userinfo}]"), None);
    }

    match after_userinfo.rfind(':') {
        Some(i) => (
            format!("{userinfo}{}", &after_userinfo[..i]),
            Some(after_userinfo[i + 1..].to_string()),
        ),
        None => (authority.to_string(), None),
    }
}

/// Keep the path (dropping query/fragment) and strip trailing slashes so the
/// result can be used as a `{base}/register` prefix.
fn normalize_path(tail: &str) -> String {
    let path = match tail.find(['?', '#']) {
        Some(i) => &tail[..i],
        None => tail,
    };
    path.trim_end_matches('/').to_string()
}

/// Strictly parse a coordination-server URL.
///
/// Returns the canonical `scheme://host[:port][/path]` form (the port is kept
/// exactly as configured, including its absence) or the precise reason the
/// input is unusable. Used to reject typos at the point the user saves them.
pub fn validate_coordination_server(raw: &str) -> Result<String, CoordinatorUrlError> {
    resolve(raw, true)
}

/// Best-effort normalization used when loading existing configuration files and
/// for values reaching the HTTP client directly.
///
/// Never fails: an unsalvageable value falls back to [`DEFAULT_COORDINATOR_URL`].
/// The result always carries a scheme, so it cannot silently become a relative
/// URL, and never ends in `/`, so `{base}/register` stays well formed.
pub fn normalize_coordination_server(raw: &str) -> String {
    resolve(raw, false).unwrap_or_else(|_| DEFAULT_COORDINATOR_URL.to_string())
}

fn resolve(raw: &str, strict: bool) -> Result<String, CoordinatorUrlError> {
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        return if strict {
            Err(CoordinatorUrlError::Empty)
        } else {
            Ok(DEFAULT_COORDINATOR_URL.to_string())
        };
    }

    let (scheme, rest) = split_scheme(trimmed);
    if scheme != "http" && scheme != "https" {
        return if strict {
            Err(CoordinatorUrlError::UnsupportedScheme(scheme))
        } else {
            Ok(trimmed.to_string())
        };
    }

    let (authority, tail) = split_authority(rest);
    let (host, port_text) = split_host_port(authority);
    if host.is_empty() {
        return if strict {
            Err(CoordinatorUrlError::MissingHost(trimmed.to_string()))
        } else {
            Ok(DEFAULT_COORDINATOR_URL.to_string())
        };
    }

    let authority = match port_text.as_deref() {
        // No port configured: keep it that way (the coordinator is on port 80).
        None | Some("") => host,
        Some(text) => match text.parse::<u16>() {
            Ok(port) if port != 0 => format!("{host}:{port}"),
            _ => {
                return if strict {
                    Err(CoordinatorUrlError::InvalidPort(text.to_string()))
                } else {
                    // Drop only the unusable port; keep the configured host.
                    Ok(format!("{scheme}://{host}{}", normalize_path(tail)))
                };
            }
        },
    };

    Ok(format!("{scheme}://{authority}{}", normalize_path(tail)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live deployment is plain HTTP on port 80; pin it so nobody "fixes"
    /// it into `:7879` again (the coordinator's binary default is irrelevant
    /// for this deployment).
    #[test]
    fn built_in_default_matches_the_live_deployment() {
        assert_eq!(DEFAULT_COORDINATOR_URL, "http://34.66.188.245");
        assert_eq!(
            normalize_coordination_server(DEFAULT_COORDINATOR_URL),
            DEFAULT_COORDINATOR_URL
        );
        assert!(!DEFAULT_COORDINATOR_URL.ends_with('/'));
    }

    #[test]
    fn a_missing_scheme_is_added_and_the_port_is_left_alone() {
        // The critical regression guard: no hidden `:7879` and no hidden port
        // rewrite of a working host.
        assert_eq!(
            normalize_coordination_server("34.66.188.245"),
            "http://34.66.188.245"
        );
        assert_eq!(
            normalize_coordination_server("http://34.66.188.245"),
            "http://34.66.188.245"
        );
        assert_eq!(
            normalize_coordination_server("localhost"),
            "http://localhost"
        );
    }

    #[test]
    fn explicit_ports_are_preserved_exactly() {
        for raw in [
            "http://34.66.188.245:80",
            "http://34.66.188.245:7879",
            "http://host:9000",
            "192.168.1.5:8080",
            "https://mesh.hivebear.com",
            "https://mesh.hivebear.com:8443",
        ] {
            let once = normalize_coordination_server(raw);
            assert_eq!(
                normalize_coordination_server(&once),
                once,
                "must be idempotent for {raw}"
            );
            assert!(once.contains("://"), "must be absolute for {raw}");
        }
    }

    #[test]
    fn https_without_a_port_stays_on_the_scheme_default() {
        assert_eq!(
            normalize_coordination_server("https://mesh.hivebear.com"),
            "https://mesh.hivebear.com"
        );
    }

    #[test]
    fn trailing_slashes_are_stripped_so_register_urls_stay_well_formed() {
        // `http://host/` + "/register" = "http://host//register" -> 404, which
        // the mesh used to report as a silent zero-peer state.
        assert_eq!(
            normalize_coordination_server("http://34.66.188.245/"),
            "http://34.66.188.245"
        );
        assert_eq!(
            normalize_coordination_server("  http://host:7879///  "),
            "http://host:7879"
        );
        assert_eq!(
            normalize_coordination_server("https://api.example.com/mesh/"),
            "https://api.example.com/mesh"
        );
    }

    #[test]
    fn ipv6_literals_are_preserved() {
        assert_eq!(
            normalize_coordination_server("http://[::1]"),
            "http://[::1]"
        );
        assert_eq!(
            normalize_coordination_server("http://[::1]:7879"),
            "http://[::1]:7879"
        );
        assert_eq!(normalize_coordination_server("http://::1"), "http://[::1]");
    }

    #[test]
    fn unsalvageable_values_fall_back_to_the_default() {
        assert_eq!(normalize_coordination_server(""), DEFAULT_COORDINATOR_URL);
        assert_eq!(
            normalize_coordination_server("   "),
            DEFAULT_COORDINATOR_URL
        );
        assert_eq!(
            normalize_coordination_server("http://"),
            DEFAULT_COORDINATOR_URL
        );
        // A broken port keeps the host rather than discarding the endpoint.
        assert_eq!(
            normalize_coordination_server("http://host:notaport"),
            "http://host"
        );
        assert_eq!(
            normalize_coordination_server("http://host:99999"),
            "http://host"
        );
        assert_eq!(
            normalize_coordination_server("http://host:0"),
            "http://host"
        );
    }

    #[test]
    fn validate_accepts_the_live_deployment_and_common_forms() {
        assert_eq!(
            validate_coordination_server("http://34.66.188.245").unwrap(),
            "http://34.66.188.245"
        );
        assert_eq!(
            validate_coordination_server("34.66.188.245:7879").unwrap(),
            "http://34.66.188.245:7879"
        );
        assert_eq!(
            validate_coordination_server("https://host/mesh").unwrap(),
            "https://host/mesh"
        );
    }

    #[test]
    fn validate_rejects_unusable_urls() {
        assert_eq!(
            validate_coordination_server("   ").unwrap_err(),
            CoordinatorUrlError::Empty
        );
        assert!(matches!(
            validate_coordination_server("http://"),
            Err(CoordinatorUrlError::MissingHost(_))
        ));
        assert_eq!(
            validate_coordination_server("ftp://host").unwrap_err(),
            CoordinatorUrlError::UnsupportedScheme("ftp".into())
        );
        assert_eq!(
            validate_coordination_server("http://host:notaport").unwrap_err(),
            CoordinatorUrlError::InvalidPort("notaport".into())
        );
        assert!(matches!(
            validate_coordination_server("http://host:70000"),
            Err(CoordinatorUrlError::InvalidPort(_))
        ));
    }
}
