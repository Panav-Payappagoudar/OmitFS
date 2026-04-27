/// Local HTTP server + embedded web UI.
///
/// `omitfs serve [--port 3030]` starts a local web server at http://localhost:<port>
/// with a beautiful semantic search interface.
///
/// Endpoints:
///   GET  /                      → Web UI (HTML)
///   GET  /api/search?q=...      → JSON array of { filename, path }
///   POST /api/ask               → JSON body { question, model? } → streaming SSE answer

use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response, Sse},
    routing::{get, post},
    Json, Router,
};
use axum::response::sse::Event as SseEvent;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::config::Config;
use crate::db::OmitDb;
use crate::embedding::EmbeddingEngine;
use crate::reranker;

// ─── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub db:     Arc<OmitDb>,
    pub engine: Arc<std::sync::Mutex<EmbeddingEngine>>,
    pub cfg:    Arc<Config>,
}

// ─── Request / Response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchQuery { q: String, limit: Option<usize> }

#[derive(Serialize)]
struct SearchResult { filename: String, path: String }

#[derive(Deserialize)]
struct AskBody { question: String, model: Option<String> }

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/",            get(serve_ui))
        .route("/api/search",  get(handle_search))
        .route("/api/ask",     post(handle_ask))
        .with_state(state)
}

// ─── UI ───────────────────────────────────────────────────────────────────────

async fn serve_ui() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        UI_HTML,
    )
}

// ─── /api/search ─────────────────────────────────────────────────────────────

async fn handle_search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(state.cfg.max_results);

    let vector = {
        let mut eng = match state.engine.lock() {
            Ok(g)  => g,
            Err(p) => p.into_inner(),
        };
        match eng.embed(&params.q) {
            Ok(v)  => v,
            Err(e) => {
                error!("Embed error: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    };

    match state.db.search_with_chunks(vector, limit, state.cfg.overfetch_factor).await {
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Ok(raw) => {
            let reranked = reranker::rerank(&params.q, raw);
            let results: Vec<SearchResult> = reranked.into_iter()
                .map(|(filename, path)| SearchResult { filename, path })
                .collect();
            Json(results).into_response()
        }
    }
}

// ─── /api/ask (SSE streaming) ─────────────────────────────────────────────────

async fn handle_ask(
    State(state): State<AppState>,
    Json(body): Json<AskBody>,
) -> Response {
    let model = body.model.unwrap_or_else(|| state.cfg.ollama_model.clone());

    // Embed question
    let vector = {
        let mut eng = match state.engine.lock() {
            Ok(g)  => g,
            Err(p) => p.into_inner(),
        };
        match eng.embed(&body.question) {
            Ok(v)  => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    };

    // Fetch context chunks
    let chunks = match state.db.search_with_chunks(vector, state.cfg.max_results, state.cfg.overfetch_factor).await {
        Ok(c)  => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let context = chunks.iter().enumerate()
        .map(|(i, (fname, _, chunk))| format!("[Source {}] ({})\n{}", i + 1, fname, chunk))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let prompt = format!(
        "You are a helpful assistant. Use ONLY the context below to answer.\n\n\
         CONTEXT:\n{context}\n\nQUESTION: {}\n\nANSWER:",
        body.question
    );

    let ollama_url = format!("{}/api/generate", state.cfg.ollama_url.trim_end_matches('/'));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<SseEvent, Infallible>>(64);

    tokio::spawn(async move {
        #[derive(serde::Serialize)]
        struct Req<'a> { model: &'a str, prompt: String, stream: bool }
        #[derive(serde::Deserialize)]
        struct Chunk { response: String, done: bool }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .unwrap();

        let resp = match client.post(&ollama_url)
            .json(&Req { model: &model, prompt, stream: true })
            .send().await
        {
            Ok(r)  => r,
            Err(e) => {
                let _ = tx.send(Ok(SseEvent::default().data(format!("ERROR: {e}")))).await;
                return;
            }
        };

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        while let Some(item) = stream.next().await {
            let bytes = match item {
                Ok(b)  => b,
                Err(_) => break,
            };
            for line in bytes.split(|&b| b == b'\n') {
                if line.is_empty() { continue; }
                if let Ok(chunk) = serde_json::from_slice::<Chunk>(line) {
                    let _ = tx.send(Ok(SseEvent::default().data(chunk.response.clone()))).await;
                    if chunk.done { return; }
                }
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).into_response()
}

// ─── Embedded HTML UI ─────────────────────────────────────────────────────────

const UI_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>OmitFS — Semantic File Search</title>
<style>
  @import url('https://fonts.googleapis.com/css2?family=Inter:wght@300;400;500;600;700&display=swap');
  :root{--bg:#0a0a0f;--surface:#12121a;--card:#1a1a28;--border:#2a2a3d;--accent:#7c6af7;--accent2:#a855f7;--text:#e8e8f0;--muted:#6b6b8a;--green:#22c55e;--red:#ef4444}
  *{box-sizing:border-box;margin:0;padding:0}
  body{background:var(--bg);color:var(--text);font-family:'Inter',sans-serif;min-height:100vh;display:flex;flex-direction:column;align-items:center;padding:2rem 1rem}
  h1{font-size:2.2rem;font-weight:700;background:linear-gradient(135deg,var(--accent),var(--accent2));-webkit-background-clip:text;-webkit-text-fill-color:transparent;margin-bottom:.3rem}
  .sub{color:var(--muted);font-size:.9rem;margin-bottom:2.5rem}
  .search-wrap{width:100%;max-width:720px;position:relative;margin-bottom:1.5rem}
  #q{width:100%;padding:1rem 3.5rem 1rem 1.2rem;background:var(--card);border:1.5px solid var(--border);border-radius:12px;color:var(--text);font-size:1.05rem;font-family:inherit;outline:none;transition:border .2s}
  #q:focus{border-color:var(--accent)}
  .search-icon{position:absolute;right:1rem;top:50%;transform:translateY(-50%);color:var(--muted);pointer-events:none;font-size:1.2rem}
  .tabs{display:flex;gap:.5rem;margin-bottom:1.5rem;background:var(--card);border-radius:10px;padding:.3rem;width:100%;max-width:720px}
  .tab{flex:1;padding:.5rem;text-align:center;border-radius:8px;cursor:pointer;font-size:.9rem;font-weight:500;color:var(--muted);transition:all .2s}
  .tab.active{background:var(--accent);color:#fff}
  #results{width:100%;max-width:720px}
  .card{background:var(--card);border:1px solid var(--border);border-radius:12px;padding:1rem 1.2rem;margin-bottom:.75rem;display:flex;align-items:center;gap:1rem;cursor:pointer;transition:border .2s,transform .15s}
  .card:hover{border-color:var(--accent);transform:translateY(-1px)}
  .file-icon{font-size:1.6rem;flex-shrink:0}
  .file-info{flex:1;min-width:0}
  .file-name{font-weight:600;font-size:.97rem;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .file-path{color:var(--muted);font-size:.8rem;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;margin-top:.2rem}
  .empty{text-align:center;color:var(--muted);padding:3rem 0;font-size:.95rem}
  #ask-box{width:100%;max-width:720px;display:none;flex-direction:column;gap:1rem}
  #ask-input{width:100%;padding:1rem 1.2rem;background:var(--card);border:1.5px solid var(--border);border-radius:12px;color:var(--text);font-size:1rem;font-family:inherit;outline:none;resize:vertical;min-height:80px;transition:border .2s}
  #ask-input:focus{border-color:var(--accent)}
  #ask-btn{padding:.75rem 1.8rem;background:linear-gradient(135deg,var(--accent),var(--accent2));border:none;border-radius:10px;color:#fff;font-size:1rem;font-weight:600;cursor:pointer;align-self:flex-start;transition:opacity .2s}
  #ask-btn:hover{opacity:.85}
  #answer-box{background:var(--card);border:1px solid var(--border);border-radius:12px;padding:1.2rem;white-space:pre-wrap;font-size:.93rem;line-height:1.7;min-height:60px;display:none}
  .spinner{width:20px;height:20px;border:2px solid var(--border);border-top-color:var(--accent);border-radius:50%;animation:spin .8s linear infinite;display:inline-block;vertical-align:middle;margin-right:.5rem}
  @keyframes spin{to{transform:rotate(360deg)}}
  .status{font-size:.82rem;color:var(--muted);margin-bottom:.5rem}
</style>
</head>
<body>
<h1>OmitFS</h1>
<p class="sub">Intent-driven local semantic search</p>

<div class="tabs">
  <div class="tab active" onclick="switchTab('search')">🔍 Search Files</div>
  <div class="tab" onclick="switchTab('ask')">🤖 Ask AI</div>
</div>

<div class="search-wrap" id="search-panel">
  <input id="q" type="text" placeholder='Search your files — e.g. "calculus assignment"' autocomplete="off"/>
  <span class="search-icon">⌕</span>
</div>
<div id="results"><p class="empty">Start typing to search your local files…</p></div>

<div id="ask-box">
  <textarea id="ask-input" placeholder='Ask anything about your files — e.g. "What formula did I derive in my calculus notes?"'></textarea>
  <button id="ask-btn" onclick="doAsk()">Ask AI ✦</button>
  <div class="status" id="ask-status"></div>
  <div id="answer-box"></div>
</div>

<script>
const extIcon = e => ({'pdf':'📄','docx':'📝','doc':'📝','xlsx':'📊','xls':'📊','csv':'📊','mp3':'🎵','mp4':'🎬','mov':'🎬','png':'🖼','jpg':'🖼','jpeg':'🖼','zip':'📦','rs':'🦀','py':'🐍','js':'🟨','ts':'🔷','html':'🌐','md':'📋','txt':'📃'}[e] || '📁');

let timer;
document.getElementById('q').addEventListener('input', () => {
  clearTimeout(timer);
  const q = document.getElementById('q').value.trim();
  if (!q) { document.getElementById('results').innerHTML='<p class="empty">Start typing to search your local files…</p>'; return; }
  document.getElementById('results').innerHTML='<p class="empty"><span class="spinner"></span>Searching…</p>';
  timer = setTimeout(() => search(q), 300);
});

async function search(q) {
  try {
    const r = await fetch(`/api/search?q=${encodeURIComponent(q)}`);
    const data = await r.json();
    const box = document.getElementById('results');
    if (!data.length) { box.innerHTML='<p class="empty">No files matched. Drop files into ~/.omitfs_data/raw/ and run the daemon.</p>'; return; }
    box.innerHTML = data.map(f => {
      const ext = f.filename.split('.').pop().toLowerCase();
      return `<div class="card" onclick="copyPath('${f.path.replace(/'/g,"\\'")}')">
        <div class="file-icon">${extIcon(ext)}</div>
        <div class="file-info">
          <div class="file-name">${f.filename}</div>
          <div class="file-path">${f.path}</div>
        </div>
      </div>`;
    }).join('');
  } catch(e) { document.getElementById('results').innerHTML=`<p class="empty">Error: ${e.message}</p>`; }
}

function copyPath(p) {
  navigator.clipboard.writeText(p).then(() => {
    const el = [...document.querySelectorAll('.card')].find(c => c.innerHTML.includes(p.replace(/'/g,"\\'")));
    if(el){el.style.borderColor='var(--green)';setTimeout(()=>el.style.borderColor='',1000);}
  });
}

async function doAsk() {
  const q = document.getElementById('ask-input').value.trim();
  if (!q) return;
  const box = document.getElementById('answer-box');
  const status = document.getElementById('ask-status');
  box.style.display='block'; box.textContent='';
  status.textContent='Searching context & generating answer…';
  document.getElementById('ask-btn').disabled = true;

  try {
    const resp = await fetch('/api/ask', {
      method: 'POST',
      headers: {'Content-Type':'application/json'},
      body: JSON.stringify({ question: q })
    });
    const reader = resp.body.getReader();
    const dec = new TextDecoder();
    status.textContent = 'Streaming answer…';
    while(true) {
      const {done, value} = await reader.read();
      if(done) break;
      const text = dec.decode(value);
      // SSE: parse data: lines
      text.split('\n').forEach(line => {
        if(line.startsWith('data:')) box.textContent += line.slice(5);
      });
    }
    status.textContent = 'Done.';
  } catch(e) {
    box.textContent = 'Error: ' + e.message + '\n\nMake sure Ollama is running: https://ollama.com';
    status.textContent = '';
  }
  document.getElementById('ask-btn').disabled = false;
}

function switchTab(t) {
  document.querySelectorAll('.tab').forEach((el,i) => el.classList.toggle('active', (t==='search'&&i===0)||(t==='ask'&&i===1)));
  document.getElementById('search-panel').style.display = t==='search'?'block':'none';
  document.getElementById('results').style.display = t==='search'?'block':'none';
  document.getElementById('ask-box').style.display = t==='ask'?'flex':'none';
}
switchTab('search');
</script>
</body>
</html>"#;
