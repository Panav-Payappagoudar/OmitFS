use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Retrieval-Augmented Generation via a locally-running Ollama instance.
///
/// Ollama is 100 % local — no API keys, no internet after model download.
/// Install: https://ollama.com  then: `ollama pull llama3`
///
/// If Ollama is not running the command prints a clear install message.

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model:  &'a str,
    prompt: String,
    stream: bool,
}

#[derive(Deserialize)]
struct GenerateChunk {
    response: String,
    done:     bool,
}

/// Ask a question using `context_chunks` as grounding context.
/// Streams the answer token-by-token to stdout.
pub async fn ask(
    question:       &str,
    context_chunks: &[(String, String, String)], // (filename, path, chunk_text)
    ollama_url:     &str,
    model:          &str,
) -> Result<()> {
    // Build RAG context from retrieved chunks
    let context = context_chunks
        .iter()
        .enumerate()
        .map(|(i, (fname, _, chunk))| {
            format!("[Source {}] ({})\n{}", i + 1, fname, chunk)
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let prompt = format!(
        "You are a helpful assistant with access to the user's local files.\n\
         Use ONLY the provided context to answer. If the answer is not in the context, say so clearly.\n\n\
         CONTEXT:\n{context}\n\n\
         QUESTION: {question}\n\n\
         ANSWER:"
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("Failed to build HTTP client")?;

    let url = format!("{}/api/generate", ollama_url.trim_end_matches('/'));

    let resp = client
        .post(&url)
        .json(&GenerateRequest { model, prompt, stream: true })
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                anyhow::anyhow!(
                    "Cannot reach Ollama at {url}.\n\
                     Install  : https://ollama.com\n\
                     Then run : ollama pull {model}\n\
                     Then run : ollama serve\n\
                     Error    : {e}"
                )
            } else {
                anyhow::anyhow!("Ollama request failed: {e}")
            }
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body   = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama returned HTTP {status}: {body}");
    }

    use tokio::io::AsyncWriteExt;
    let mut stdout = tokio::io::stdout();
    let mut bytes  = resp.bytes_stream();

    use futures::StreamExt;
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.context("Stream read error")?;
        // Each line is a JSON object
        for line in chunk.split(|&b| b == b'\n') {
            if line.is_empty() { continue; }
            if let Ok(parsed) = serde_json::from_slice::<GenerateChunk>(line) {
                stdout.write_all(parsed.response.as_bytes()).await?;
                stdout.flush().await?;
                if parsed.done { break; }
            }
        }
    }
    println!(); // final newline
    Ok(())
}

/// List models currently available in the local Ollama instance.
pub async fn list_models(ollama_url: &str) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Model  { name: String }
    #[derive(Deserialize)]
    struct Models { models: Vec<Model> }

    let url    = format!("{}/api/tags", ollama_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp   = client.get(&url).send().await
        .context("Cannot reach Ollama — is it running?")?;
    let body: Models = resp.json().await.context("Failed to parse Ollama model list")?;
    Ok(body.models.into_iter().map(|m| m.name).collect())
}
