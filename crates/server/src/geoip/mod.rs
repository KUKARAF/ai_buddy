//! Offline, best-effort IP -> country pre-fill for the per-user COUNTRY setting.
//!
//! This module is OPTIONAL and fully degradable: when no database is configured
//! (`Config::geoip_db_path` is `None`), or on ANY lookup failure, every function
//! simply returns `None` and the user sets their country by hand. It never
//! panics.
//!
//! The database is a MaxMind-DB-format file, either:
//!   - **DB-IP IP-to-Country Lite** ("IP Geolocation by DB-IP", CC-BY-4.0 — the
//!     attribution "IP Geolocation by DB-IP" must be preserved wherever this
//!     data is used), or
//!   - a MaxMind **GeoLite2 Country** `.mmdb`.
//!
//! Privacy: the client IP is used ONLY for the in-memory country lookup. It is
//! never logged, persisted, or sent anywhere — only the resolved ISO-3166-1
//! alpha-2 country code (e.g. "DE") is ever surfaced.

use std::net::IpAddr;
use std::sync::OnceLock;

use axum::http::HeaderMap;

use crate::config::Config;

/// Lazily-opened reader for the single configured database path. Opened at most
/// once for the process; `None` means "no db configured" or "open failed", both
/// of which degrade to no pre-fill.
static READER: OnceLock<Option<maxminddb::Reader<Vec<u8>>>> = OnceLock::new();

/// Borrow the process-wide reader, opening it from `path` on first use.
fn reader(path: &str) -> Option<&'static maxminddb::Reader<Vec<u8>>> {
    READER
        .get_or_init(|| match maxminddb::Reader::open_readfile(path) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path,
                    "geoip: failed to open database; IP pre-fill disabled"
                );
                None
            }
        })
        .as_ref()
}

/// Best-effort client IP from reverse-proxy headers.
///
/// Prefers the FIRST entry of `X-Forwarded-For` (the original client, ahead of
/// any intermediate proxies), falling back to `X-Real-IP`. Both are set by our
/// reverse proxy (Caddy). Returns `None` on any missing/malformed header.
pub fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let raw = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })?;
    raw.parse::<IpAddr>().ok()
}

/// Suggest an ISO-3166-1 alpha-2 country (uppercased) for the caller, derived
/// from their IP via the offline database. Returns `None` when geoip is not
/// configured or anything at all goes wrong.
pub fn suggested_country(config: &Config, headers: &HeaderMap) -> Option<String> {
    let path = config.geoip_db_path.as_deref()?;
    let reader = reader(path)?;
    let ip = client_ip(headers)?;
    let record: maxminddb::geoip2::Country = reader.lookup(ip).ok()?;
    let iso = record.country.and_then(|c| c.iso_code)?;
    let iso = iso.trim();
    if iso.is_empty() {
        return None;
    }
    Some(iso.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn client_ip_prefers_first_xff_entry() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 70.41.3.18, 150.172.238.178"),
        );
        let expected: IpAddr = "203.0.113.7".parse().expect("parse");
        assert_eq!(client_ip(&h), Some(expected));
    }

    #[test]
    fn client_ip_trims_whitespace_around_entries() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("  198.51.100.9  ,  10.0.0.1"),
        );
        let expected: IpAddr = "198.51.100.9".parse().expect("parse");
        assert_eq!(client_ip(&h), Some(expected));
    }

    #[test]
    fn client_ip_falls_back_to_x_real_ip() {
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", HeaderValue::from_static("2001:db8::1"));
        let expected: IpAddr = "2001:db8::1".parse().expect("parse");
        assert_eq!(client_ip(&h), Some(expected));
    }

    #[test]
    fn client_ip_none_when_absent() {
        let empty = HeaderMap::new();
        assert_eq!(client_ip(&empty), None);
    }

    #[test]
    fn client_ip_none_on_garbage() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(client_ip(&h), None);
    }

    #[test]
    fn client_ip_none_on_empty_header() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static(""));
        assert_eq!(client_ip(&h), None);
    }

    #[test]
    fn suggested_country_none_when_db_unconfigured() {
        // With no geoip_db_path there is never a suggestion, regardless of IP.
        let mut config = Config::from_env();
        config.geoip_db_path = None;
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", HeaderValue::from_static("8.8.8.8"));
        assert_eq!(suggested_country(&config, &h), None);
    }
}
