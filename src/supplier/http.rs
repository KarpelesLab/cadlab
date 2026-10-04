//! Minimal HTTP transport for network providers, so their request logic can be tested with a
//! mock instead of the network.
//!
//! [`Ureq`] is the real transport (blocking, rustls, 30 s timeout). Tests pass any closure
//! `Fn(&Request) -> Result<Response, String>` as a [`Transport`].

use std::time::Duration;

/// Request body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// `application/json`.
    Json(String),
    /// `application/x-www-form-urlencoded`.
    Form(Vec<(String, String)>),
}

/// A POST request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Full URL, including any query string.
    pub url: String,
    /// Extra headers (`Content-Type` is set from the body).
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Body,
}

impl Request {
    /// A JSON POST.
    pub fn json(url: impl Into<String>, body: &serde_json::Value) -> Self {
        Request { url: url.into(), headers: Vec::new(), body: Body::Json(body.to_string()) }
    }

    /// A form POST.
    pub fn form(url: impl Into<String>, fields: &[(&str, &str)]) -> Self {
        Request {
            url: url.into(),
            headers: Vec::new(),
            body: Body::Form(fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()),
        }
    }

    /// Adds a header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// A response: status and body text. HTTP error statuses are responses, not errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// Body text.
    pub body: String,
}

/// Sends requests. Errors are transport failures (DNS, TLS, timeout), as text.
pub trait Transport: Send + Sync {
    /// Sends a POST request.
    fn post(&self, req: &Request) -> Result<Response, String>;
}

impl<F> Transport for F
where
    F: Fn(&Request) -> Result<Response, String> + Send + Sync,
{
    fn post(&self, req: &Request) -> Result<Response, String> {
        self(req)
    }
}

/// The real transport (`ureq`).
pub struct Ureq(ureq::Agent);

impl Default for Ureq {
    fn default() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        Ureq(agent)
    }
}

impl Transport for Ureq {
    fn post(&self, req: &Request) -> Result<Response, String> {
        let mut b = self.0.post(&req.url).header("Accept", "application/json");
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        let mut resp = match &req.body {
            Body::Json(s) => b.header("Content-Type", "application/json").send(s.as_str()),
            Body::Form(f) => b.send_form(f.iter().map(|(k, v)| (k.as_str(), v.as_str()))),
        }
        .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
        Ok(Response { status, body })
    }
}

/// Removes `secret` from a message (transport errors may echo the URL, which can carry an API
/// key in its query string).
pub fn redact(message: &str, secret: &str) -> String {
    if secret.is_empty() { message.to_string() } else { message.replace(secret, "***") }
}
