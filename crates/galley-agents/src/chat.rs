//! An OpenAI-compatible chat backend with tool calling. One code path covers Ollama, LM Studio,
//! llama.cpp, vLLM and hosted providers. The differences are handled at the edges. Tool-call ids
//! may be missing, and `arguments` may arrive as an object instead of a string.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("could not reach the model at {url}: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("the model endpoint answered HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unexpected reply from the model endpoint: {0}")]
    Shape(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_kind() -> String {
    "function".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Kept as the string the API defines. Some servers send an object, which the client re-encodes.
    #[serde(default, deserialize_with = "arguments_as_string")]
    pub arguments: String,
}

fn arguments_as_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::String(s) => s,
        Value::Null => "{}".into(),
        other => other.to_string(),
    })
}

impl Message {
    pub fn system(text: impl Into<String>) -> Message {
        Message { role: "system".into(), content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn user(text: impl Into<String>) -> Message {
        Message { role: "user".into(), content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn tool(id: &str, text: impl Into<String>) -> Message {
        Message { role: "tool".into(), content: Some(text.into()), tool_calls: None, tool_call_id: Some(id.to_string()) }
    }
}

pub struct ChatBackend {
    http: reqwest::Client,
    url: String,
    model: String,
    api_key: Option<String>,
}

impl ChatBackend {
    /// `endpoint` is the base URL ending in `/v1`, e.g. `http://localhost:11434/v1`.
    pub fn new(endpoint: &str, model: &str, api_key: Option<String>) -> ChatBackend {
        ChatBackend {
            http: reqwest::Client::new(),
            url: format!("{}/chat/completions", endpoint.trim_end_matches('/')),
            model: model.to_string(),
            api_key,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn complete(&self, messages: &[Message], tools: &Value) -> Result<Message, ChatError> {
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "temperature": 0,
        });
        let mut req = self.http.post(&self.url).json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|source| ChatError::Http { url: self.url.clone(), source })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| ChatError::Shape(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(ChatError::Status { status, body: text.chars().take(400).collect() });
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| ChatError::Shape(e.to_string()))?;
        let msg = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .cloned()
            .ok_or_else(|| ChatError::Shape("no choices[0].message".into()))?;
        let mut msg: Message = serde_json::from_value(msg).map_err(|e| ChatError::Shape(e.to_string()))?;
        // Tool results are matched to calls by id. The client makes an id when the server sent none.
        if let Some(calls) = msg.tool_calls.as_mut() {
            for (i, c) in calls.iter_mut().enumerate() {
                if c.id.is_empty() {
                    c.id = format!("call_{i}");
                }
            }
            if calls.is_empty() {
                msg.tool_calls = None;
            }
        }
        Ok(msg)
    }
}
