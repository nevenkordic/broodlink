/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! MCP method dispatch and beads-bridge HTTP client.

use crate::protocol;
use serde_json::json;
use tracing::{info, warn};

/// Bridge client for proxying tool calls to beads-bridge.
pub struct BridgeClient {
    http: reqwest::Client,
    bridge_url: String,
    jwt_token: String,
    status_api_url: String,
    status_api_key: String,
}

impl BridgeClient {
    pub fn new(
        bridge_url: String,
        jwt_token: String,
        status_api_url: String,
        status_api_key: String,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            bridge_url,
            jwt_token,
            status_api_url,
            status_api_key,
        }
    }

    /// Fetch tool definitions from beads-bridge and convert to MCP format.
    pub async fn list_tools(&self) -> Result<serde_json::Value, (i32, String)> {
        let url = format!("{}/api/v1/tools", self.bridge_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| (protocol::INTERNAL_ERROR, format!("bridge request failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| (protocol::INTERNAL_ERROR, format!("bridge response parse error: {e}")))?;

        // Convert from OpenAI function-calling format to MCP tool format
        let tools = body["tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|t| convert_tool_to_mcp(t))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(json!({ "tools": tools }))
    }

    /// Call a tool via beads-bridge.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, (i32, String)> {
        let url = format!("{}/api/v1/tool/{}", self.bridge_url, name);

        let body = json!({ "params": arguments });

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.jwt_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| (protocol::INTERNAL_ERROR, format!("bridge request failed: {e}")))?;

        let status = resp.status();
        let response_body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| (protocol::INTERNAL_ERROR, format!("bridge response parse error: {e}")))?;

        if !status.is_success() {
            let error_msg = response_body["error"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string();
            return Ok(json!({
                "content": [{
                    "type": "text",
                    "text": format!("Error: {error_msg}")
                }],
                "isError": true,
            }));
        }

        // MCP tool result format
        let data = &response_body["data"];
        let text = serde_json::to_string_pretty(data)
            .unwrap_or_else(|_| data.to_string());

        Ok(json!({
            "content": [{
                "type": "text",
                "text": text,
            }],
        }))
    }

    /// List available resources from status-api.
    pub fn list_resources(&self) -> serde_json::Value {
        json!({
            "resources": [
                {
                    "uri": "broodlink://agents",
                    "name": "Agent Profiles",
                    "description": "All registered agent profiles and their status",
                    "mimeType": "application/json",
                },
                {
                    "uri": "broodlink://tasks",
                    "name": "Task Queue",
                    "description": "Current task queue entries",
                    "mimeType": "application/json",
                },
                {
                    "uri": "broodlink://approvals",
                    "name": "Approval Gates",
                    "description": "Pending and resolved approval gates",
                    "mimeType": "application/json",
                },
                {
                    "uri": "broodlink://metrics",
                    "name": "Agent Metrics",
                    "description": "Agent performance metrics and routing scores",
                    "mimeType": "application/json",
                },
                {
                    "uri": "broodlink://guardrails",
                    "name": "Guardrail Policies",
                    "description": "Active guardrail policies and recent violations",
                    "mimeType": "application/json",
                },
                {
                    "uri": "broodlink://summary",
                    "name": "Daily Summary",
                    "description": "Recent daily activity summaries",
                    "mimeType": "application/json",
                },
            ]
        })
    }

    /// Read a resource from status-api.
    pub async fn read_resource(
        &self,
        uri: &str,
    ) -> Result<serde_json::Value, (i32, String)> {
        let endpoint = match uri {
            "broodlink://agents" => "/api/v1/agents",
            "broodlink://tasks" => "/api/v1/tasks",
            "broodlink://approvals" => "/api/v1/approvals",
            "broodlink://metrics" => "/api/v1/agent-metrics",
            "broodlink://guardrails" => "/api/v1/guardrails",
            "broodlink://summary" => "/api/v1/summary",
            _ => {
                return Err((
                    protocol::INVALID_REQUEST,
                    format!("unknown resource URI: {uri}"),
                ))
            }
        };

        let url = format!("{}{}", self.status_api_url, endpoint);
        let resp = self
            .http
            .get(&url)
            .header("X-Broodlink-Api-Key", &self.status_api_key)
            .send()
            .await
            .map_err(|e| (protocol::INTERNAL_ERROR, format!("status-api request failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| (protocol::INTERNAL_ERROR, format!("status-api response parse error: {e}")))?;

        let text = serde_json::to_string_pretty(&body)
            .unwrap_or_else(|_| body.to_string());

        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": text,
            }]
        }))
    }
}

/// Convert an OpenAI function-calling tool definition to MCP format.
fn convert_tool_to_mcp(openai_tool: &serde_json::Value) -> serde_json::Value {
    let func = &openai_tool["function"];
    json!({
        "name": func["name"],
        "description": func["description"],
        "inputSchema": func["parameters"],
    })
}

/// Dispatch an MCP JSON-RPC request to the appropriate handler.
pub async fn dispatch(
    client: &BridgeClient,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, (i32, String)> {
    info!(method = %method, "MCP method dispatch");

    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false },
            },
            "serverInfo": {
                "name": "broodlink-mcp-server",
                "version": env!("CARGO_PKG_VERSION"),
            }
        })),

        "notifications/initialized" => {
            info!("MCP client initialized");
            Ok(json!({}))
        }

        "tools/list" => client.list_tools().await,

        "tools/call" => {
            let name = params["name"]
                .as_str()
                .ok_or_else(|| (protocol::INVALID_REQUEST, "missing tool name".to_string()))?;
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            client.call_tool(name, &arguments).await
        }

        "resources/list" => Ok(client.list_resources()),

        "resources/read" => {
            let uri = params["uri"]
                .as_str()
                .ok_or_else(|| (protocol::INVALID_REQUEST, "missing resource URI".to_string()))?;
            client.read_resource(uri).await
        }

        "prompts/list" => Ok(json!({
            "prompts": [
                {
                    "name": "agent-status",
                    "description": "Get a summary of all active agents and their current status",
                },
                {
                    "name": "task-overview",
                    "description": "Get an overview of pending and in-progress tasks",
                },
            ]
        })),

        "prompts/get" => {
            let name = params["name"]
                .as_str()
                .ok_or_else(|| (protocol::INVALID_REQUEST, "missing prompt name".to_string()))?;

            match name {
                "agent-status" => Ok(json!({
                    "description": "Agent status summary",
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": "List all active agents with their roles, last seen time, and current task load. Use the list_agents and get_routing_scores tools."
                        }
                    }]
                })),
                "task-overview" => Ok(json!({
                    "description": "Task queue overview",
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": "Show all pending and in-progress tasks with their priorities and assigned agents. Use the list_tasks tool."
                        }
                    }]
                })),
                _ => Err((protocol::INVALID_REQUEST, format!("unknown prompt: {name}"))),
            }
        }

        "ping" => Ok(json!({})),

        _ => {
            warn!(method = %method, "unknown MCP method");
            Err((protocol::METHOD_NOT_FOUND, format!("unknown method: {method}")))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_tool_to_mcp() {
        let openai = json!({
            "type": "function",
            "function": {
                "name": "ping",
                "description": "Check connectivity",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": [],
                }
            }
        });
        let mcp = convert_tool_to_mcp(&openai);
        assert_eq!(mcp["name"], "ping");
        assert_eq!(mcp["description"], "Check connectivity");
        assert_eq!(mcp["inputSchema"]["type"], "object");
    }

    #[test]
    fn test_list_resources_has_entries() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake-token".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let resources = client.list_resources();
        let arr = resources["resources"].as_array().unwrap();
        assert!(arr.len() >= 5, "should have at least 5 resources");
        // Verify all have required fields
        for r in arr {
            assert!(r["uri"].is_string());
            assert!(r["name"].is_string());
            assert!(r["mimeType"].is_string());
        }
    }

    #[tokio::test]
    async fn test_dispatch_initialize() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake-token".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let result = dispatch(&client, "initialize", &json!({})).await;
        let data = result.unwrap();
        assert_eq!(data["protocolVersion"], "2024-11-05");
        assert!(data["capabilities"]["tools"].is_object());
        assert_eq!(data["serverInfo"]["name"], "broodlink-mcp-server");
    }

    #[tokio::test]
    async fn test_dispatch_ping() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake-token".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let result = dispatch(&client, "ping", &json!({})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_dispatch_unknown_method() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake-token".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let result = dispatch(&client, "nonexistent/method", &json!({})).await;
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, protocol::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dispatch_prompts_list() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake-token".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let result = dispatch(&client, "prompts/list", &json!({})).await;
        let data = result.unwrap();
        let prompts = data["prompts"].as_array().unwrap();
        assert!(prompts.len() >= 2);
    }

    #[tokio::test]
    async fn test_dispatch_prompts_get() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake-token".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let result = dispatch(&client, "prompts/get", &json!({"name": "agent-status"})).await;
        let data = result.unwrap();
        assert!(data["messages"].is_array());
    }

    #[tokio::test]
    async fn test_dispatch_resources_list() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake-token".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let result = dispatch(&client, "resources/list", &json!({})).await;
        let data = result.unwrap();
        assert!(data["resources"].is_array());
    }
}
