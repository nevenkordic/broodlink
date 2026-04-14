/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Formalized streaming event protocol for Broodlink.
//!
//! Provides a unified event envelope and strongly-typed event kinds used
//! across all services (beads-bridge SSE, a2a-gateway webhooks, coordinator
//! NATS topics, and the Python SDK). Every event shares the same envelope
//! structure so consumers can deserialize any stream with one parser.
//!
//! # Event lifecycle for a typical tool invocation
//!
//! ```text
//! StreamStart → ToolMatch → ToolExecStart → MessageDelta* → ToolExecEnd → StreamEnd
//! ```
//!
//! # Event lifecycle for a task dispatch
//!
//! ```text
//! StreamStart → RouteStart → AgentMatch → TaskDispatched → MessageDelta* → TaskCompleted → StreamEnd
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Core envelope — every event on every transport uses this shape
// ---------------------------------------------------------------------------

/// Universal event envelope wrapping all Broodlink stream events.
///
/// The `stream_id` groups events belonging to the same logical operation
/// (e.g. a single tool call or a full task lifecycle). `sequence` provides
/// total ordering within a stream so consumers can detect gaps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Unique identifier for this individual event.
    pub event_id: Uuid,
    /// Logical stream this event belongs to.
    pub stream_id: String,
    /// Monotonically increasing sequence number within the stream (0-based).
    pub sequence: u64,
    /// ISO-8601 timestamp of when the event was created.
    pub timestamp: DateTime<Utc>,
    /// The service that emitted this event.
    pub source: EventSource,
    /// Typed event payload.
    pub kind: EventKind,
}

impl StreamEvent {
    /// Create a new event in the given stream with an auto-generated id and
    /// the current UTC timestamp.
    pub fn new(
        stream_id: impl Into<String>,
        sequence: u64,
        source: EventSource,
        kind: EventKind,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            stream_id: stream_id.into(),
            sequence,
            timestamp: Utc::now(),
            source,
            kind,
        }
    }

    /// Serialize to a JSON string suitable for SSE `data:` lines or NATS payloads.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The SSE `event:` field value derived from the event kind discriminant.
    pub fn sse_event_type(&self) -> &'static str {
        self.kind.event_type()
    }
}

// ---------------------------------------------------------------------------
// Event source — which service emitted the event
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    BeadsBridge,
    Coordinator,
    A2aGateway,
    StatusApi,
    EmbeddingWorker,
    Heartbeat,
    Agent(String),
}

// ---------------------------------------------------------------------------
// Typed event kinds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    // -- Stream lifecycle --
    /// First event in every stream. Carries metadata about the operation.
    StreamStart(StreamStartData),
    /// Final event in a successful stream.
    StreamEnd(StreamEndData),
    /// Terminal event when the stream ends due to an error.
    StreamError(StreamErrorData),

    // -- Routing events (coordinator) --
    /// Coordinator has begun routing a task to an agent.
    RouteStart(RouteStartData),
    /// An agent candidate was evaluated during routing.
    AgentMatch(AgentMatchData),
    /// Task has been dispatched to the selected agent.
    TaskDispatched(TaskDispatchedData),
    /// Task completed successfully.
    TaskCompleted(TaskCompletedData),
    /// Task failed.
    TaskFailed(TaskFailedData),

    // -- Tool events (beads-bridge) --
    /// A tool was matched for the incoming request.
    ToolMatch(ToolMatchData),
    /// Tool execution has started.
    ToolExecStart(ToolExecStartData),
    /// Tool execution finished.
    ToolExecEnd(ToolExecEndData),

    // -- Content events --
    /// Incremental content chunk (LLM token stream, partial results).
    MessageDelta(MessageDeltaData),
    /// A complete message (non-streaming contexts).
    MessageComplete(MessageCompleteData),

    // -- Permission events --
    /// A tool or action was blocked by a permission policy.
    PermissionDenied(PermissionDeniedData),
    /// A guardrail policy was triggered.
    GuardrailTriggered(GuardrailTriggeredData),

    // -- Budget events --
    /// Agent budget was debited for a tool call.
    BudgetDebit(BudgetDebitData),
    /// Agent budget was exhausted.
    BudgetExhausted(BudgetExhaustedData),
}

impl EventKind {
    /// Returns a stable string identifier for use as the SSE `event:` field.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::StreamStart(_) => "stream_start",
            Self::StreamEnd(_) => "stream_end",
            Self::StreamError(_) => "stream_error",
            Self::RouteStart(_) => "route_start",
            Self::AgentMatch(_) => "agent_match",
            Self::TaskDispatched(_) => "task_dispatched",
            Self::TaskCompleted(_) => "task_completed",
            Self::TaskFailed(_) => "task_failed",
            Self::ToolMatch(_) => "tool_match",
            Self::ToolExecStart(_) => "tool_exec_start",
            Self::ToolExecEnd(_) => "tool_exec_end",
            Self::MessageDelta(_) => "message_delta",
            Self::MessageComplete(_) => "message_complete",
            Self::PermissionDenied(_) => "permission_denied",
            Self::GuardrailTriggered(_) => "guardrail_triggered",
            Self::BudgetDebit(_) => "budget_debit",
            Self::BudgetExhausted(_) => "budget_exhausted",
        }
    }
}

// ---------------------------------------------------------------------------
// Data payloads for each event kind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStartData {
    /// Human-readable description of the operation (e.g. "tool:store_memory", "task:abc123").
    pub operation: String,
    /// The agent initiating or receiving this stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Optional metadata bag (formula params, tool params, etc.).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEndData {
    /// Total duration of the stream in milliseconds.
    pub duration_ms: u64,
    /// Summary status.
    pub status: StreamStatus,
    /// Optional result summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamErrorData {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recoverable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamStatus {
    Success,
    Partial,
    Failed,
    Cancelled,
}

// -- Routing payloads --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteStartData {
    pub task_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula_name: Option<String>,
    pub candidate_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMatchData {
    pub task_id: String,
    pub agent_id: String,
    pub score: f64,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub breakdown: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDispatchedData {
    pub task_id: String,
    pub agent_id: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletedData {
    pub task_id: String,
    pub agent_id: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub result_summary: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFailedData {
    pub task_id: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub retryable: bool,
}

// -- Tool payloads --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMatchData {
    pub tool_name: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecStartData {
    pub tool_name: String,
    pub agent_id: String,
    pub trace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecEndData {
    pub tool_name: String,
    pub agent_id: String,
    pub trace_id: String,
    pub duration_ms: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// -- Content payloads --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDeltaData {
    /// Incremental text chunk.
    pub content: String,
    /// Running token count for this stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    /// Role of the message author (assistant, tool, system).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageCompleteData {
    pub content: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

// -- Permission payloads --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDeniedData {
    pub tool_name: String,
    pub agent_id: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailTriggeredData {
    pub policy_name: String,
    pub agent_id: String,
    pub severity: String,
    pub message: String,
}

// -- Budget payloads --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetDebitData {
    pub agent_id: String,
    pub tool_name: String,
    pub cost: i64,
    pub remaining_balance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetExhaustedData {
    pub agent_id: String,
    pub balance: i64,
    pub required: i64,
}

// ---------------------------------------------------------------------------
// Stream builder — helper for services emitting ordered event sequences
// ---------------------------------------------------------------------------

/// Tracks sequence numbering for a single event stream.
///
/// # Usage
/// ```rust,no_run
/// use broodlink_events::*;
/// let mut builder = StreamBuilder::new("my-stream-123", EventSource::BeadsBridge);
/// let start = builder.emit(EventKind::StreamStart(StreamStartData {
///     operation: "tool:store_memory".into(),
///     agent_id: Some("worker-1".into()),
///     metadata: serde_json::Value::Null,
/// }));
/// // send `start.to_json()` over SSE / NATS
/// ```
pub struct StreamBuilder {
    stream_id: String,
    source: EventSource,
    sequence: u64,
}

impl StreamBuilder {
    pub fn new(stream_id: impl Into<String>, source: EventSource) -> Self {
        Self {
            stream_id: stream_id.into(),
            source,
            sequence: 0,
        }
    }

    /// Create the next event in sequence.
    pub fn emit(&mut self, kind: EventKind) -> StreamEvent {
        let event = StreamEvent::new(&self.stream_id, self.sequence, self.source.clone(), kind);
        self.sequence += 1;
        event
    }

    /// Current sequence counter (next event will get this number).
    pub fn next_sequence(&self) -> u64 {
        self.sequence
    }

    /// The stream ID.
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_builder_sequences_correctly() {
        let mut builder = StreamBuilder::new("test-stream", EventSource::BeadsBridge);

        let e0 = builder.emit(EventKind::StreamStart(StreamStartData {
            operation: "test".into(),
            agent_id: None,
            metadata: serde_json::Value::Null,
        }));
        assert_eq!(e0.sequence, 0);
        assert_eq!(e0.stream_id, "test-stream");

        let e1 = builder.emit(EventKind::StreamEnd(StreamEndData {
            duration_ms: 42,
            status: StreamStatus::Success,
            summary: None,
        }));
        assert_eq!(e1.sequence, 1);
    }

    #[test]
    fn event_serializes_to_tagged_json() {
        let event = StreamEvent::new(
            "s1",
            0,
            EventSource::Coordinator,
            EventKind::ToolMatch(ToolMatchData {
                tool_name: "store_memory".into(),
                agent_id: "worker-1".into(),
                match_reason: None,
            }),
        );

        let json = event.to_json();
        assert!(json.contains("\"type\":\"tool_match\""));
        assert!(json.contains("store_memory"));
    }

    #[test]
    fn sse_event_type_matches_discriminant() {
        let kind = EventKind::MessageDelta(MessageDeltaData {
            content: "hello".into(),
            token_count: Some(1),
            role: None,
        });
        assert_eq!(kind.event_type(), "message_delta");
    }
}
