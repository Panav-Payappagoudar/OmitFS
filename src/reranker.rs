/// BM25 re-ranker for semantic search results.
///
/// After vector search returns the top-N candidates, BM25 re-scores them
/// using exact keyword overlap, term frequency, and a filename-presence boost.
/// Results that match query words in both content *and* filename rank highest.

/// Tokenise text into lowercase alphanumeric words of length ≥ 2.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_lowercase())
        .collect()
}

/// Re-rank `results` (filename, physical_path, chunk_text) against `query`.
/// Returns `(filename, physical_path, chunk_text)` in descending relevance order.
pub fn rerank(
    query:   &str,
    results: Vec<(String, String, String)>,
) -> Vec<(String, String, String)> {
    if results.is_empty() { return vec![]; }

    let query_terms = tokenize(query);
    if query_terms.is_empty() {
        return results;
    }

    let n      = results.len() as f64;
    let k1     = 1.5_f64;
    let b      = 0.75_f64;

    // Average document (chunk) length
    let avg_dl = results.iter()
        .map(|(_, _, text)| tokenize(text).len())
        .sum::<usize>() as f64
        / n;

    // IDF per query term across all candidate chunks
    let idf: std::collections::HashMap<String, f64> = query_terms.iter().map(|term| {
        let df = results.iter()
            .filter(|(_, _, text)| text.to_lowercase().contains(term.as_str()))
            .count() as f64;
        let score = ((n - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);
        (term.clone(), score)
    }).collect();

    let mut scored: Vec<(f64, String, String, String)> = results
        .into_iter()
        .map(|(fname, path, text)| {
            let tokens = tokenize(&text);
            let dl     = tokens.len() as f64;

            let bm25_score: f64 = query_terms.iter().map(|term| {
                let tf       = tokens.iter().filter(|t| *t == term).count() as f64;
                let idf_val  = idf.get(term).copied().unwrap_or(0.0);
                let denom    = tf + k1 * (1.0 - b + b * dl / avg_dl.max(1.0));
                idf_val * (tf * (k1 + 1.0)) / denom.max(f64::EPSILON)
            }).sum();

            // Boost when query terms appear in the filename itself
            let fname_boost: f64 = query_terms.iter()
                .filter(|t| fname.to_lowercase().contains(t.as_str()))
                .count() as f64 * 3.0;

            (bm25_score + fname_boost, fname, path, text)
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(_, f, p, t)| (f, p, t)).collect()
}
