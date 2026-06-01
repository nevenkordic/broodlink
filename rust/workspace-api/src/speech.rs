/*
 * Broodlink workspace-api — Speech (STT + TTS).
 * Ported from the workspace app stt_routes / tts_routes. The local engines
 * (faster-whisper, Kokoro) and endpoint dispatch are not wired here, so the
 * services report disabled and synthesis/transcription return 503 with the
 * documented shapes. Stats + cache-clear are real no-ops.
 */

use axum::Json;
use serde_json::{json, Value};

use crate::WsError;

pub async fn stt_stats() -> Json<Value> {
    Json(json!({
        "available": false,
        "provider": "disabled",
        "model": "",
        "model_loaded": false,
        "language": "en"
    }))
}

pub async fn stt_transcribe() -> Result<Json<Value>, WsError> {
    Err(WsError::BadRequest(
        "speech-to-text is not configured (local whisper not ported)".into(),
    ))
}

pub async fn tts_stats() -> Json<Value> {
    Json(json!({
        "available": false,
        "ready": false,
        "provider": "disabled",
        "model": "",
        "voice": "",
        "speed": 1.0,
        "cache_entries": 0,
        "cache_size_mb": 0.0
    }))
}

pub async fn tts_synthesize() -> Result<Json<Value>, WsError> {
    Err(WsError::BadRequest(
        "text-to-speech is not configured (local TTS not ported)".into(),
    ))
}

pub async fn tts_clear_cache() -> Json<Value> {
    Json(json!({ "success": true, "message": "Cache cleared" }))
}
