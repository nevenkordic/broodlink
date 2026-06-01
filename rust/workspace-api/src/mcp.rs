/*
 * Broodlink workspace-api — MCP client.
 *
 * Talks JSON-RPC to Broodlink's mcp-server (which proxies beads-bridge's ~96
 * tools: memory, tasks, agents, messaging, knowledge graph, scheduling…). This
 * is what makes the workspace agent a first-class Broodlink agent — it shares
 * the same tools and memory as every other agent in the fleet.
 *
 * Config (env): WORKSPACE_MCP_URL (default http://localhost:3311/mcp),
 * WORKSPACE_MCP_TOKEN (optional bearer). Streamable-HTTP MCP: we `initialize`
 * once to obtain an Mcp-Session-Id, then issue tools/list and tools/call.
 */

use serde_json::{json, Value};
use tokio::sync::Mutex;

const PROTOCOL_VERSION: &str = "2025-03-26";

pub struct McpClient {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
    session: Mutex<Option<String>>,
}

impl McpClient {
    pub fn from_env() -> Self {
        let base = std::env::var("WORKSPACE_MCP_URL")
            .unwrap_or_else(|_| "http://localhost:3311/mcp".to_string());
        let token = std::env::var("WORKSPACE_MCP_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        Self {
            base,
            token,
            http: reqwest::Client::new(),
            session: Mutex::new(None),
        }
    }

    fn req(&self, session: Option<&str>) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .post(&self.base)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        if let Some(s) = session {
            r = r.header("Mcp-Session-Id", s);
        }
        r
    }

    /// Parse an MCP HTTP response that may be plain JSON or a single SSE frame.
    fn extract_json(body: &str) -> Result<Value, String> {
        let trimmed = body.trim();
        if trimmed.starts_with('{') {
            return serde_json::from_str(trimmed).map_err(|e| e.to_string());
        }
        for line in trimmed.lines() {
            if let Some(d) = line.strip_prefix("data:") {
                let d = d.trim();
                if d.starts_with('{') {
                    return serde_json::from_str(d).map_err(|e| e.to_string());
                }
            }
        }
        Err("no JSON in MCP response".into())
    }

    async fn ensure_session(&self) -> Result<String, String> {
        {
            let g = self.session.lock().await;
            if let Some(s) = g.as_ref() {
                return Ok(s.clone());
            }
        }
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "broodlink-workspace", "version": "0.1" }
            }
        });
        let resp = self
            .req(None)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let sid = resp
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_default();
        // Some servers are stateless and return no session id; that's fine.
        let mut g = self.session.lock().await;
        *g = Some(sid.clone());
        Ok(sid)
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let sid = self.ensure_session().await?;
        let sref = if sid.is_empty() {
            None
        } else {
            Some(sid.as_str())
        };
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
        let resp = self
            .req(sref)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let v = Self::extract_json(&text)?;
        if let Some(err) = v.get("error") {
            return Err(err.to_string());
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Raw MCP tool definitions ({name, description, inputSchema}).
    pub async fn list_tools(&self) -> Result<Vec<Value>, String> {
        let result = self.rpc("tools/list", json!({})).await?;
        Ok(result["tools"].as_array().cloned().unwrap_or_default())
    }

    /// Call a tool; returns the concatenated text content of the result.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<String, String> {
        let result = self
            .rpc("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        Ok(content_text(&result))
    }
}

/// Flatten an MCP tool result's `content` array into a single text string.
pub fn content_text(result: &Value) -> String {
    if let Some(arr) = result["content"].as_array() {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|c| {
                if c["type"] == "text" {
                    c["text"].as_str().map(String::from)
                } else {
                    Some(c.to_string())
                }
            })
            .collect();
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    // Fallback: stringify whatever came back.
    result.to_string()
}

/// Convert MCP tool defs into OpenAI function-tool schemas.
pub fn to_openai_tools(mcp_tools: &[Value]) -> Vec<Value> {
    mcp_tools
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": t["description"].as_str().unwrap_or(""),
                    "parameters": t.get("inputSchema").cloned().unwrap_or(json!({"type":"object"})),
                }
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_tool_schema() {
        let mcp = vec![json!({
            "name": "store_memory",
            "description": "Store a memory",
            "inputSchema": {"type":"object","properties":{"content":{"type":"string"}}}
        })];
        let oa = to_openai_tools(&mcp);
        assert_eq!(oa[0]["type"], "function");
        assert_eq!(oa[0]["function"]["name"], "store_memory");
        assert_eq!(
            oa[0]["function"]["parameters"]["properties"]["content"]["type"],
            "string"
        );
    }

    #[test]
    fn flattens_content() {
        let r =
            json!({ "content": [{"type":"text","text":"hello"},{"type":"text","text":"world"}] });
        assert_eq!(content_text(&r), "hello\nworld");
    }

    #[test]
    fn parses_sse_framed_json() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
        let v = McpClient::extract_json(body).unwrap();
        assert!(v["result"]["tools"].is_array());
    }
}
