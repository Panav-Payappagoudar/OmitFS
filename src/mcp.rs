/// MCP (Model Context Protocol) stdio server.
///
/// Exposes OmitFS search and RAG capabilities as tools that any MCP-compatible
/// AI agent (Claude Desktop, Cursor, Continue, etc.) can call natively.
///
/// Usage: pipe `omitfs mcp` into your agent's MCP configuration.
/// Protocol: JSON-RPC 2.0 over stdin/stdout (one JSON object per line).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::error;

use crate::config::Config;
use crate::db::OmitDb;
use crate::embedding::EmbeddingEngine;
use crate::reranker;

// ─── JSON-RPC types ───────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct RpcRequest {
    #[serde(default)]
    id:     Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id:      Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result:  Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error:   Option<Value>,
}

fn ok(id: Value, result: Value) -> RpcResponse {
    RpcResponse { jsonrpc: "2.0", id, result: Some(result), error: None }
}

fn err(id: Value, code: i64, msg: &str) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0", id,
        result:  None,
        error:   Some(json!({ "code": code, "message": msg })),
    }
}

// ─── Tool manifest ────────────────────────────────────────────────────────────

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "search",
                "description": "Semantic search over the user's locally-indexed files. Returns matching file names and paths.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Natural-language search query" },
                        "limit": { "type": "integer", "description": "Max results (default 10)", "default": 10 }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "ask",
                "description": "Ask a question answered from the user's local files using RAG + Ollama.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "The question to answer" },
                        "model":    { "type": "string", "description": "Ollama model to use (default: llama3)", "default": "llama3" }
                    },
                    "required": ["question"]
                }
            }
        ]
    })
}

// ─── Server loop ──────────────────────────────────────────────────────────────

pub async fn run_mcp_server(
    db:     Arc<OmitDb>,
    engine: Arc<std::sync::Mutex<EmbeddingEngine>>,
    cfg:    Arc<Config>,
) -> Result<()> {
    let stdin  = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    // Announce server info on stderr (agents read stdout only)
    eprintln!("OmitFS MCP server ready (JSON-RPC 2.0 over stdio)");

    while let Ok(Some(line)) = reader.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() { continue; }

        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Err(e) => err(Value::Null, -32700, &format!("Parse error: {e}")),
            Ok(req) => handle(&req, &db, &engine, &cfg).await,
        };

        let mut out = serde_json::to_string(&response).unwrap_or_default();
        out.push('\n');
        stdout.write_all(out.as_bytes()).await?;
        stdout.flush().await?;
    }

    Ok(())
}

// ─── Method dispatcher ────────────────────────────────────────────────────────

async fn handle(
    req:    &RpcRequest,
    db:     &OmitDb,
    engine: &Arc<std::sync::Mutex<EmbeddingEngine>>,
    cfg:    &Config,
) -> RpcResponse {
    match req.method.as_str() {
        // MCP handshake
        "initialize" => ok(req.id.clone(), json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "omitfs", "version": env!("CARGO_PKG_VERSION") }
        })),

        "tools/list" => ok(req.id.clone(), tools_list()),

        "tools/call" => {
            let name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = req.params.get("arguments").cloned().unwrap_or(json!({}));

            match name {
                "search" => handle_search(req.id.clone(), &args, db, engine, cfg).await,
                "ask"    => handle_ask(req.id.clone(), &args, db, engine, cfg).await,
                other    => err(req.id.clone(), -32601, &format!("Unknown tool: {other}")),
            }
        }

        "notifications/initialized" => ok(req.id.clone(), json!(null)),

        other => err(req.id.clone(), -32601, &format!("Method not found: {other}")),
    }
}

// ─── Tool implementations ─────────────────────────────────────────────────────

async fn handle_search(
    id:     Value,
    args:   &Value,
    db:     &OmitDb,
    engine: &Arc<std::sync::Mutex<EmbeddingEngine>>,
    cfg:    &Config,
) -> RpcResponse {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q.to_string(),
        None    => return err(id, -32602, "Missing 'query' argument"),
    };
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(cfg.max_results as u64) as usize;

    let vector = {
        let mut eng = match engine.lock() {
            Ok(g)  => g,
            Err(p) => p.into_inner(),
        };
        match eng.embed(&query) {
            Ok(v)  => v,
            Err(e) => return err(id, -32000, &format!("Embedding failed: {e}")),
        }
    };

    match db.search_with_chunks(vector, limit, cfg.overfetch_factor).await {
        Err(e) => err(id, -32000, &format!("Search failed: {e}")),
        Ok(raw) => {
            let reranked = reranker::rerank(&query, raw);
            let content: Vec<Value> = reranked.iter().map(|(fname, path)| {
                json!({ "filename": fname, "path": path })
            }).collect();
            ok(id, json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&content).unwrap_or_default() }] }))
        }
    }
}

async fn handle_ask(
    id:     Value,
    args:   &Value,
    db:     &OmitDb,
    engine: &Arc<std::sync::Mutex<EmbeddingEngine>>,
    cfg:    &Config,
) -> RpcResponse {
    let question = match args.get("question").and_then(|v| v.as_str()) {
        Some(q) => q.to_string(),
        None    => return err(id, -32602, "Missing 'question' argument"),
    };
    let model = args.get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(&cfg.ollama_model)
        .to_string();

    let vector = {
        let mut eng = match engine.lock() {
            Ok(g)  => g,
            Err(p) => p.into_inner(),
        };
        match eng.embed(&question) {
            Ok(v)  => v,
            Err(e) => return err(id, -32000, &format!("Embedding failed: {e}")),
        }
    };

    let chunks = match db.search_with_chunks(vector, cfg.max_results, cfg.overfetch_factor).await {
        Ok(c)  => c,
        Err(e) => return err(id, -32000, &format!("Search failed: {e}")),
    };

    // For MCP we collect the full answer (no streaming into a terminal)
    let context = chunks.iter().enumerate()
        .map(|(i, (fname, _, chunk))| format!("[Source {}] ({})\n{}", i + 1, fname, chunk))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let prompt = format!(
        "You are a helpful assistant with access to the user's local files.\n\
         Use ONLY the provided context. If the answer is not present, say so.\n\n\
         CONTEXT:\n{context}\n\nQUESTION: {question}\n\nANSWER:"
    );

    #[derive(serde::Serialize)]
    struct Req<'a> { model: &'a str, prompt: String, stream: bool }
    #[derive(serde::Deserialize)]
    struct Resp { response: String }

    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build() {
        Ok(c)  => c,
        Err(e) => return err(id, -32000, &format!("HTTP client error: {e}")),
    };

    let url = format!("{}/api/generate", cfg.ollama_url.trim_end_matches('/'));
    let resp = match client.post(&url).json(&Req { model: &model, prompt, stream: false }).send().await {
        Ok(r)  => r,
        Err(e) => return err(id, -32000, &format!("Ollama unreachable: {e}")),
    };

    match resp.json::<Resp>().await {
        Ok(r)  => ok(id, json!({ "content": [{ "type": "text", "text": r.response }] })),
        Err(e) => err(id, -32000, &format!("Ollama response parse error: {e}")),
    }
}
