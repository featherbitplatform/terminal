//! fbterm's only network access outside SSH: one small HTTPS GET helper, used by the update
//! check (`update.rs`) and meant for the theme gallery too. Every use is something the user
//! switched on; nothing a remote program does can reach it.
//!
//! [`get`] fetches a fixed `https://` URL with rustls certificate verification against the
//! bundled webpki roots (no OpenSSL, no system library), a whole-request timeout of
//! [`TIMEOUT`], and a cap on the response size. Redirects are followed only over HTTPS and
//! only to the start URL's exact host and port, compared as written: a start URL with an
//! explicit `:443` refuses a redirect to the same host without the port (and the other way
//! round), so write URLs the way the server redirects (normally without a port). It blocks: call it on the blocking pool (or a thread of its own),
//! never on the UI thread.

use std::time::Duration;

/// Longest a request may take, from connecting to the last byte of the body.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Most redirects followed (each within the same host).
const MAX_REDIRECTS: usize = 5;

/// GET `url` (`https://` only) with these extra `headers` and return the body of a `200`
/// response, at most `max_bytes` long. A failure (connection, TLS, a status other than 200, a
/// redirect elsewhere, a body over `max_bytes`) is returned as a short error string for the
/// log. Blocking.
pub fn get(url: &str, headers: &[(&str, &str)], max_bytes: u64) -> Result<Vec<u8>, String> {
    let host = https_host(url).ok_or_else(|| format!("not an https URL: {url}"))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(TIMEOUT))
        // Redirects are followed by hand below, so none can leave the host.
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let mut request = agent.get(&current);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.call().map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("HTTP {status} without a location"))?;
            current = same_host_redirect(&host, location)
                .ok_or_else(|| format!("redirect to another host refused: {location}"))?;
            continue;
        }
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        return response
            .body_mut()
            .with_config()
            .limit(max_bytes)
            .read_to_vec()
            .map_err(|e| e.to_string());
    }
    Err("too many redirects".into())
}

/// The host (with any port) of an `https://` URL, lowercased.
fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty() && !host.contains('@')).then(|| host.to_ascii_lowercase())
}

/// Where a redirect from `host` to `location` goes, if it stays on `host` over HTTPS: an
/// absolute `https://` URL on the same host and port as written (`host` includes any port),
/// or a path on it (`/...`).
fn same_host_redirect(host: &str, location: &str) -> Option<String> {
    if location.starts_with("//") {
        return same_host_redirect(host, &format!("https:{location}"));
    }
    if location.starts_with('/') {
        return Some(format!("https://{host}{location}"));
    }
    (https_host(location)? == host).then(|| location.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_have_a_host() {
        assert_eq!(
            https_host("https://api.github.com/repos/x").as_deref(),
            Some("api.github.com")
        );
        assert_eq!(
            https_host("https://API.github.com:443?q").as_deref(),
            Some("api.github.com:443")
        );
        assert_eq!(https_host("http://api.github.com/"), None);
        assert_eq!(https_host("https://user@evil.example/"), None);
        assert_eq!(https_host("https:///path"), None);
    }

    #[test]
    fn redirects_stay_on_the_host() {
        let host = "api.github.com";
        assert_eq!(
            same_host_redirect(host, "/repositories/1/releases/latest").as_deref(),
            Some("https://api.github.com/repositories/1/releases/latest")
        );
        assert_eq!(
            same_host_redirect(host, "https://api.github.com/x").as_deref(),
            Some("https://api.github.com/x")
        );
        assert_eq!(
            same_host_redirect(host, "//api.github.com/x").as_deref(),
            Some("https://api.github.com/x")
        );
        assert_eq!(same_host_redirect(host, "https://evil.example/x"), None);
        assert_eq!(same_host_redirect(host, "//evil.example/x"), None);
        assert_eq!(same_host_redirect(host, "http://api.github.com/x"), None);
        assert_eq!(same_host_redirect(host, "relative/path"), None);
        // Host and port compare as written.
        assert_eq!(
            same_host_redirect("api.github.com:443", "https://api.github.com/x"),
            None
        );
    }

    #[test]
    fn plain_http_is_refused_before_any_connection() {
        assert!(get("http://example.com/", &[], 10).is_err());
    }
}
