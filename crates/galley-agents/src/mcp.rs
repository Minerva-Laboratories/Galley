//! A minimal MCP client over Streamable HTTP. It has enough of JSON-RPC to initialize, fetch a
//! prompt, and call tools against one project's `/mcp/{id}` endpoint.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("could not reach {url}: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("the server rejected the token or the project (HTTP {0}); check --url and --token")]
    Status(u16),
    /// HTTP 429, or the JSON-RPC rate-limit error. The runner does not retry.
    #[error("the server is rate-limiting this account; try again in a moment")]
    RateLimited,
    #[error("{0}")]
    Rpc(String),
    #[error("unexpected response from the server: {0}")]
    Shape(String),
}

pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

pub struct McpClient {
    http: reqwest::Client,
    url: String,
    token: String,
    next_id: AtomicU64,
}

impl McpClient {
    /// `url` is the project's MCP endpoint as shown in the Share panel.
    pub fn new(url: &str, token: &str) -> McpClient {
        McpClient {
            http: reqwest::Client::new(),
            url: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    async fn post(&self, body: Value) -> Result<Option<Value>, McpError> {
        let resp = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|source| McpError::Http { url: self.url.clone(), source })?;
        let status = resp.status();
        if status.as_u16() == 202 {
            return Ok(None);
        }
        if status.as_u16() == 429 {
            return Err(McpError::RateLimited);
        }
        if !status.is_success() {
            return Err(McpError::Status(status.as_u16()));
        }
        let v: Value = resp.json().await.map_err(|e| McpError::Shape(e.to_string()))?;
        Ok(Some(v))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let v = self.post(msg).await?.ok_or_else(|| McpError::Shape("no response body".into()))?;
        if let Some(err) = v.get("error") {
            return Err(rpc_error(err));
        }
        v.get("result").cloned().ok_or_else(|| McpError::Shape("missing result".into()))
    }

    /// Handshake. Returns the server's instructions, which carry the project's `AGENTS.md`.
    pub async fn initialize(&self) -> Result<String, McpError> {
        let r = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "galley-runner", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        // A notification has no id and expects no reply. The server answers 202.
        let _ = self.post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;
        Ok(r.get("instructions").and_then(Value::as_str).unwrap_or("").to_string())
    }

    pub async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult, McpError> {
        let r = self.request("tools/call", json!({ "name": name, "arguments": arguments })).await?;
        let text = r
            .get("content")
            .and_then(Value::as_array)
            .map(|c| c.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        let is_error = r.get("isError").and_then(Value::as_bool).unwrap_or(false);
        Ok(ToolResult { text, is_error })
    }

    /// The text of a built-in prompt, with the project's conventions already appended.
    pub async fn prompt(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        let r = self.request("prompts/get", json!({ "name": name, "arguments": arguments })).await?;
        r.get("messages")
            .and_then(Value::as_array)
            .and_then(|m| m.first())
            .and_then(|m| m.get("content"))
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| McpError::Shape("prompt has no text".into()))
    }
}

/// The server answers every rate-limit refusal with this JSON-RPC code and its 429 text.
const RATE_LIMITED: i64 = -32000;

fn rpc_error(err: &Value) -> McpError {
    let m = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
    if err.get("code").and_then(Value::as_i64) == Some(RATE_LIMITED) && m.starts_with("Too many attempts") {
        return McpError::RateLimited;
    }
    McpError::Rpc(m.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limit_reads_as_one() {
        let limited = json!({ "code": -32000, "message": "Too many attempts. Try again in a few moments." });
        assert!(matches!(rpc_error(&limited), McpError::RateLimited));
        assert_eq!(McpError::RateLimited.to_string(), "the server is rate-limiting this account; try again in a moment");
        let other = json!({ "code": -32601, "message": "Method not found: x" });
        assert!(matches!(rpc_error(&other), McpError::Rpc(m) if m == "Method not found: x"));
        let other_server = json!({ "code": -32000, "message": "something else" });
        assert!(matches!(rpc_error(&other_server), McpError::Rpc(_)));
    }
}
