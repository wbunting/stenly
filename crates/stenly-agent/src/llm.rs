//! OpenAI-compatible chat completions with tool calling (Ollama, OpenAI,
//! Anthropic's compatibility endpoint, OpenRouter, local routers, …).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use stenly_core::config::LlmTarget;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: String,
    /// `null` on an assistant message that only calls tools.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: "system".into(), content: Some(text.into()), tool_calls: vec![], tool_call_id: None }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: "user".into(), content: Some(text.into()), tool_calls: vec![], tool_call_id: None }
    }
    pub fn tool(id: &str, text: impl Into<String>) -> Self {
        Self { role: "tool".into(), content: Some(text.into()), tool_calls: vec![], tool_call_id: Some(id.to_string()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    /// JSON text (some servers send an object; normalised to text on parse).
    #[serde(default, deserialize_with = "args_text")]
    pub arguments: String,
}

fn args_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::String(s) => s,
        Value::Null => "{}".into(),
        other => other.to_string(),
    })
}

impl ToolCall {
    pub fn args(&self) -> Value {
        let a = self.function.arguments.trim();
        if a.is_empty() {
            return json!({});
        }
        serde_json::from_str(a).unwrap_or_else(|_| json!({ "_raw": a }))
    }
}

/// A tool as offered to the model.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolSpec {
    fn to_openai(&self) -> Value {
        json!({ "type": "function", "function": { "name": self.name, "description": self.description, "parameters": self.parameters } })
    }
}

/// Something that can answer one chat turn (the HTTP client, or a script in tests).
pub trait ChatModel: Send + Sync {
    fn chat(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Message>;
    fn name(&self) -> String;
}

pub struct OpenAiChat {
    pub target: LlmTarget,
    pub timeout_secs: u64,
}

impl ChatModel for OpenAiChat {
    fn name(&self) -> String {
        self.target.model.clone()
    }

    fn chat(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Message> {
        let t = &self.target;
        anyhow::ensure!(t.cli.is_none(), "this model can't call tools; the context agent needs a hosted API or custom model (Settings → Context)");
        anyhow::ensure!(!t.endpoint.is_empty() && !t.model.is_empty(), "no model set up for context (Settings → Context)");
        if t.backend == stenly_core::config::Backend::ChatGpt {
            return chatgpt_turn(t, messages, tools, self.timeout_secs);
        }
        let url = format!("{}/chat/completions", t.endpoint);
        let mut body = json!({
            "model": t.model,
            "messages": messages,
            "temperature": 0.2,
            "stream": false,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.iter().map(ToolSpec::to_openai).collect());
            body["tool_choice"] = json!("auto");
        }
        if !t.reasoning_effort.is_empty() {
            body["reasoning_effort"] = json!(t.reasoning_effort);
        }
        if t.local {
            // several turns in a row: keep the model loaded between them
            body["keep_alive"] = json!("5m");
            let chars: usize = messages.iter().map(|m| m.content.as_deref().map_or(0, str::len)).sum();
            body["options"] = json!({ "num_ctx": ((chars as f64 / 3.2) as u32 + 8192).next_power_of_two().clamp(16_384, 131_072) });
        }
        let mut req = ureq::post(&url).config().timeout_global(Some(std::time::Duration::from_secs(self.timeout_secs.max(30)))).http_status_as_error(false).build();
        if let Some(key) = &t.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let mut resp = req.send_json(&body).with_context(|| format!("calling {url}"))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(str::to_string)).unwrap_or_else(|| text.chars().take(200).collect());
            match status {
                401 | 403 => bail!("the context model's API key was rejected"),
                404 => bail!("model “{}” not found at {} ({msg})", t.model, t.endpoint),
                429 => bail!("rate limited or out of credits"),
                _ => bail!("HTTP {status} from {url}: {msg}"),
            }
        }
        parse_reply(&text)
    }
}

/// One turn on the ChatGPT backend (Responses API): the conversation becomes
/// Responses input items, tool calls come back as `function_call` items.
fn chatgpt_turn(t: &LlmTarget, messages: &[Message], tools: &[ToolSpec], timeout_secs: u64) -> Result<Message> {
    let input = to_responses_input(messages);
    let tools: Vec<Value> = tools.iter().map(|s| json!({ "type": "function", "name": s.name, "description": s.description, "parameters": s.parameters, "strict": false })).collect();
    let reply = stenly_core::chatgpt::respond(t, input, &tools, timeout_secs).map_err(|e| e.context("the context model (ChatGPT) failed"))?;
    let tool_calls = reply
        .calls
        .into_iter()
        .enumerate()
        .map(|(i, c)| ToolCall { id: if c.call_id.is_empty() { format!("call_{i}") } else { c.call_id }, kind: function_type(), function: FunctionCall { name: c.name, arguments: c.arguments } })
        .collect();
    let text = stenly_core::summary::strip_think(&reply.text).trim().to_string();
    Ok(Message { role: "assistant".into(), content: Some(text).filter(|s| !s.is_empty()), tool_calls, tool_call_id: None })
}

/// Chat messages as Responses API input items.
pub fn to_responses_input(messages: &[Message]) -> Vec<Value> {
    use stenly_core::chatgpt::message;
    let mut out = Vec::new();
    for m in messages {
        let text = m.content.as_deref().unwrap_or("");
        match m.role.as_str() {
            "system" => out.push(message("developer", text)),
            "tool" => out.push(json!({ "type": "function_call_output", "call_id": m.tool_call_id.clone().unwrap_or_default(), "output": text })),
            "assistant" => {
                if !text.is_empty() {
                    out.push(message("assistant", text));
                }
                for c in &m.tool_calls {
                    out.push(json!({ "type": "function_call", "call_id": c.id, "name": c.function.name, "arguments": c.function.arguments }));
                }
            }
            _ => out.push(message("user", text)),
        }
    }
    out
}

/// The assistant message from a chat completion response.
pub fn parse_reply(text: &str) -> Result<Message> {
    let v: Value = serde_json::from_str(text).context("the model's reply wasn't JSON")?;
    let m = v["choices"][0]["message"].clone();
    anyhow::ensure!(m.is_object(), "no message in the model's reply");
    let mut msg: Message = serde_json::from_value(m).context("unexpected message shape")?;
    msg.role = "assistant".into();
    // ids are required to answer a call; some local servers leave them out
    for (i, c) in msg.tool_calls.iter_mut().enumerate() {
        if c.id.is_empty() {
            c.id = format!("call_{i}");
        }
    }
    if let Some(c) = &msg.content {
        let stripped = stenly_core::summary::strip_think(c).trim().to_string();
        msg.content = Some(stripped);
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_calls() {
        let r = r#"{"choices":[{"message":{"role":"assistant","content":"Looking.","tool_calls":[{"id":"toolu_1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"Paris\"}"}},{"function":{"name":"x","arguments":{"a":1}}}]}}]}"#;
        let m = parse_reply(r).unwrap();
        assert_eq!(m.tool_calls.len(), 2);
        assert_eq!(m.tool_calls[0].args()["city"], "Paris");
        assert_eq!(m.tool_calls[1].id, "call_1");
        assert_eq!(m.tool_calls[1].args()["a"], 1);
        // round-trips as an OpenAI assistant message
        let back = serde_json::to_value(&m).unwrap();
        assert_eq!(back["tool_calls"][0]["type"], "function");
    }

    #[test]
    fn conversation_maps_to_responses_items() {
        let call = ToolCall { id: "call_1".into(), kind: function_type(), function: FunctionCall { name: "past__search".into(), arguments: "{\"q\":\"x\"}".into() } };
        let msgs = vec![
            Message::system("Find background."),
            Message::user("Meeting: Sync"),
            Message { role: "assistant".into(), content: None, tool_calls: vec![call], tool_call_id: None },
            Message::tool("call_1", "3 results"),
        ];
        let items = to_responses_input(&msgs);
        assert_eq!(items[0]["role"], "developer");
        assert_eq!(items[1]["content"][0]["type"], "input_text");
        assert_eq!(items[2], json!({"type": "function_call", "call_id": "call_1", "name": "past__search", "arguments": "{\"q\":\"x\"}"}));
        assert_eq!(items[3], json!({"type": "function_call_output", "call_id": "call_1", "output": "3 results"}));
    }

    #[test]
    fn plain_reply_strips_thinking() {
        let m = parse_reply(r#"{"choices":[{"message":{"role":"assistant","content":"<think>hm</think> Done."}}]}"#).unwrap();
        assert_eq!(m.content.as_deref(), Some("Done."));
        assert!(m.tool_calls.is_empty());
    }
}
