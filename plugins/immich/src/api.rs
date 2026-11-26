//! Immich HTTP client (ported from Pigeon's `immich.rs`). Every call runs on a plugin worker thread.
//!
//! Policy kept from the built-in implementation: redirects are rejected (credentials are never
//! forwarded), responses are capped at 20 MiB, URLs carry no credentials, and error bodies are never
//! shown to the user (only the HTTP status).

use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
    Method, Url,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, io::Read, time::Duration};

pub type Result<T> = std::result::Result<T, String>;

/// Largest JSON or thumbnail response accepted from the server.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// Immich asset pages requested per call.
pub const PAGE_SIZE: usize = 250;
pub const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];

/// Validates and normalises a server URL: HTTP(S), no credentials, query or fragment, optional `/api`
/// suffix removed.
pub fn normalize_server_url(value: &str) -> Result<String> {
    let mut url = Url::parse(value.trim()).map_err(|_| "Enter a valid Immich server URL.".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Immich server URL must be HTTP or HTTPS without credentials, a query, or a fragment.".into());
    }
    let mut path = url.path().trim_end_matches('/').to_owned();
    if path.to_ascii_lowercase().ends_with("/api") {
        path.truncate(path.len() - 4);
    }
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Immich identifiers are canonical UUIDs. Anything else is rejected before it reaches a URL.
pub fn valid_id(value: &str) -> Result<&str> {
    let ok = value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    if !ok {
        return Err("Invalid Immich identifier.".into());
    }
    Ok(value)
}

/// Account identity: server URL plus the lower-cased email (matches the built-in client, so
/// identities survive the migration).
pub fn account_key(base: &str, email: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{base}\0{}", email.trim().to_lowercase())))
}

#[derive(Clone)]
pub struct ImmichClient {
    pub base: String,
    pub http: Client,
    pub account_key: String,
}

impl ImmichClient {
    /// Logs in with email and password (`POST /api/auth/login`) and returns a bearer-authenticated client.
    pub fn login(server_url: &str, email: &str, password: &str) -> Result<Self> {
        let base = normalize_server_url(server_url)?;
        if email.trim().is_empty() || password.is_empty() {
            return Err("An Immich email and password are required".into());
        }
        let anonymous = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Could not prepare the Immich login".to_owned())?;
        let response = anonymous
            .post(format!("{base}/api/auth/login"))
            .json(&json!({"email":email.trim(),"password":password}))
            .send()
            .map_err(|_| "Could not reach the Immich login service".to_owned())?;
        if !response.status().is_success() {
            if response.status().as_u16() == 401 {
                return Err("Immich login rejected (HTTP 401). Check the server URL, email and password, and whether password login is enabled.".into());
            }
            return Err(format!("Immich login failed (HTTP {})", response.status().as_u16()));
        }
        let mut data = Vec::new();
        response
            .take(1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|_| "Could not read the Immich login response".to_owned())?;
        if data.len() > 1024 * 1024 {
            return Err("Immich login response exceeds its safety limit".into());
        }
        let response: Value = serde_json::from_slice(&data).map_err(|_| "Invalid Immich login response".to_owned())?;
        let token = response["accessToken"].as_str().unwrap_or("");
        if token.is_empty() {
            return Err("Immich login did not return an access token".into());
        }
        let mut secret =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| "Invalid Immich access token".to_owned())?;
        secret.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("authorization", secret);
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Could not prepare the authenticated Immich client".to_owned())?;
        Ok(Self { account_key: account_key(&base, email), base, http })
    }

    /// Compatibility path kept from the built-in client (API key, headless tests only; not in the UI).
    pub fn with_api_key(server_url: &str, api_key: &str) -> Result<Self> {
        let base = normalize_server_url(server_url)?;
        let key = api_key.trim();
        if key.is_empty() || api_key.contains(['\r', '\n']) {
            return Err("An Immich API key is required.".into());
        }
        let mut headers = HeaderMap::new();
        let mut secret = HeaderValue::from_str(key).map_err(|_| "Invalid Immich API key.".to_owned())?;
        secret.set_sensitive(true);
        headers.insert("x-api-key", secret);
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Could not prepare the Immich HTTP client.".to_owned())?;
        let account_key = format!("{:x}", Sha256::digest(format!("{base}\0api-key\0{key}")));
        Ok(Self { base, http, account_key })
    }

    /// Sends one request and returns the capped body. Errors carry the HTTP status, never the body.
    pub fn request(&self, method: Method, endpoint: &str, body: Option<&Value>) -> Result<Vec<u8>> {
        let mut request = self.http.request(method, format!("{}/api{endpoint}", self.base));
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().map_err(|error| {
            if error.is_timeout() {
                "Immich request timed out.".to_owned()
            } else {
                "Could not connect to the Immich server.".to_owned()
            }
        })?;
        if !response.status().is_success() {
            return Err(format!("Immich request failed (HTTP {}).", response.status().as_u16()));
        }
        if response.content_length().is_some_and(|n| n > MAX_BYTES) {
            return Err("Immich response exceeds the size limit.".into());
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the Immich response.".to_owned())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Immich response exceeds the size limit.".into());
        }
        Ok(bytes)
    }

    pub fn json(&self, method: Method, endpoint: &str, body: Option<&Value>) -> Result<Value> {
        let bytes = self.request(method, endpoint, body)?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| "Immich returned an invalid response.".to_owned())
    }

    /// Lists every image of the account, page by page. `cancelled` is checked between pages.
    pub fn list_images(&self, cancelled: &dyn Fn() -> bool, progress: &dyn Fn(usize)) -> Result<Vec<Value>> {
        let mut assets = Vec::new();
        visit_image_pages(
            |page| {
                if cancelled() {
                    return Err("Immich sync cancelled".into());
                }
                self.json(
                    Method::POST,
                    "/search/metadata",
                    Some(&json!({"type":"IMAGE","page":page,"size":PAGE_SIZE,"withExif":true})),
                )
            },
            |items| {
                assets.try_reserve(items.len()).map_err(|_| "Not enough memory for the Immich asset listing.".to_owned())?;
                assets.extend(items);
                progress(assets.len());
                Ok(())
            },
        )?;
        Ok(assets)
    }

    pub fn list_tags(&self) -> Result<Vec<Value>> {
        self.json(Method::GET, "/tags", None)?
            .as_array()
            .cloned()
            .ok_or_else(|| "Immich returned an invalid tag list.".to_owned())
    }

    /// Assigns an existing tag to assets. Partial failures are errors, never silent successes.
    pub fn add_existing_tag(&self, tag_id: &str, asset_ids: &[String]) -> Result<()> {
        valid_id(tag_id)?;
        let mut unique: Vec<&String> = Vec::new();
        let mut seen = HashSet::new();
        for asset_id in asset_ids {
            valid_id(asset_id)?;
            if seen.insert(asset_id) {
                unique.push(asset_id);
            }
        }
        if unique.is_empty() {
            return Ok(());
        }
        let response = self.json(Method::PUT, &format!("/tags/{tag_id}/assets"), Some(&json!({"ids":unique})))?;
        if response
            .as_array()
            .is_some_and(|items| items.iter().any(|i| i["success"] == false && i["error"] != "duplicate"))
        {
            return Err("Immich could not tag every selected image. Refresh to check partial results.".into());
        }
        Ok(())
    }

    /// The preview JPEG source used for thumbnails (`size=preview`). Originals are never requested here.
    pub fn thumbnail(&self, asset_id: &str) -> Result<Vec<u8>> {
        valid_id(asset_id)?;
        self.request(Method::GET, &format!("/assets/{asset_id}/thumbnail?size=preview"), None)
    }
}

/// Walks `POST /search/metadata` pages. Detects repeated pages (a server loop) and invalid cursors.
pub fn visit_image_pages(
    mut fetch: impl FnMut(u64) -> Result<Value>,
    mut visit: impl FnMut(Vec<Value>) -> Result<()>,
) -> Result<()> {
    let mut page = 1u64;
    let mut seen = HashSet::new();
    loop {
        seen.try_reserve(1).map_err(|_| "Not enough memory to track Immich pages.".to_owned())?;
        if !seen.insert(page) {
            return Err("Immich returned a repeated page.".into());
        }
        let mut response = fetch(page)?;
        let items = response["assets"]["items"]
            .take()
            .as_array_mut()
            .map(std::mem::take)
            .ok_or_else(|| "Immich returned an invalid asset page.".to_owned())?;
        visit(items)?;
        let next = &response["assets"]["nextPage"];
        if next.is_null() || next == "" {
            return Ok(());
        }
        page = next
            .as_u64()
            .or_else(|| next.as_str().and_then(|s| s.parse().ok()))
            .filter(|p| *p > 0)
            .ok_or_else(|| "Immich returned an invalid next page.".to_owned())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_normalised_and_unsafe_forms_rejected() {
        assert_eq!(normalize_server_url("https://example.test/immich/api/").unwrap(), "https://example.test/immich");
        for url in [
            "file:///tmp/x",
            "https://user:secret@example.test",
            "https://example.test/?secret=1",
            "https://example.test/#x",
            "not a url",
        ] {
            assert!(normalize_server_url(url).is_err(), "{url} must be rejected");
        }
    }

    #[test]
    fn identifiers_must_be_canonical_uuids() {
        assert!(valid_id("11111111-1111-4111-8111-111111111111").is_ok());
        assert!(valid_id("../../etc/passwd").is_err());
        assert!(valid_id("11111111-1111-4111-8111-11111111111g").is_err());
    }

    #[test]
    fn account_identity_is_case_insensitive_on_email() {
        assert_eq!(account_key("https://example.test", "A@EXAMPLE.TEST"), account_key("https://example.test", "a@example.test"));
        assert_ne!(account_key("https://example.test", "a@example.test"), account_key("https://other.test", "a@example.test"));
    }

    #[test]
    fn repeated_pages_are_rejected() {
        let error = visit_image_pages(
            |_| Ok(json!({"assets":{"items":[],"nextPage":1}})),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("repeated page"));
    }

    #[test]
    fn pages_are_followed_until_null() {
        let mut count = 0usize;
        visit_image_pages(
            |page| Ok(json!({"assets":{"items":vec![Value::Null; 100], "nextPage": if page < 10_001 { Some(page + 1) } else { None }}})),
            |items| {
                count += items.len();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(count, 1_000_100);
    }
}
