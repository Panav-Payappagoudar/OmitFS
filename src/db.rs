use anyhow::{Context, Result};
use arrow_array::{
    Float32Array, FixedSizeListArray, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::StreamExt;
use lancedb::query::ExecutableQuery;
use std::path::PathBuf;
use std::sync::Arc;

// Embedding dimension for all-MiniLM-L6-v2
const VEC_DIM: i32 = 384;

pub struct OmitDb {
    table: lancedb::Table,
}

impl OmitDb {
    pub async fn init(db_path: PathBuf) -> Result<Self> {
        let uri = db_path
            .to_str()
            .context("Invalid db path (non-UTF8)")?
            .to_string();

        let conn = lancedb::connect(&uri)
            .execute()
            .await
            .context("Failed to connect to LanceDB")?;

        let schema = Arc::new(Schema::new(vec![
            Field::new("file_id",       DataType::Utf8, false),
            Field::new("filename",      DataType::Utf8, false),
            Field::new("physical_path", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    VEC_DIM,
                ),
                false,
            ),
        ]));

        let table = match conn.open_table("files").execute().await {
            Ok(t) => t,
            Err(_) => {
                // Table doesn't exist yet — create it with empty schema
                let empty = RecordBatchIterator::new(
                    std::iter::empty::<Result<RecordBatch, arrow_schema::ArrowError>>(),
                    schema.clone(),
                );
                conn.create_table("files", Box::new(empty))
                    .execute()
                    .await
                    .context("Failed to create LanceDB table")?
            }
        };

        Ok(Self { table })
    }

    // ─── Insert ──────────────────────────────────────────────────────────────

    pub async fn insert_file(
        &self,
        file_id: &str,
        filename: &str,
        physical_path: &str,
        vector: Vec<f32>,
    ) -> Result<()> {
        let schema = self.table.schema().await.context("Failed to get table schema")?;

        let ids    = Arc::new(StringArray::from(vec![file_id]));
        let names  = Arc::new(StringArray::from(vec![filename]));
        let paths  = Arc::new(StringArray::from(vec![physical_path]));

        let flat      = Float32Array::from(vector);
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vecs      = Arc::new(FixedSizeListArray::try_new(
            list_field,
            VEC_DIM,
            Arc::new(flat),
            None,
        ).context("Failed to build FixedSizeListArray")?);

        let batch = RecordBatch::try_new(schema, vec![ids, names, paths, vecs])
            .context("Failed to build RecordBatch")?;

        let reader = RecordBatchIterator::new(
            vec![Ok(batch)].into_iter(),
            self.table.schema().await?,
        );

        self.table
            .add(Box::new(reader))
            .execute()
            .await
            .context("Failed to insert into LanceDB")?;

        Ok(())
    }

    // ─── Delete by physical path ──────────────────────────────────────────────
    // Removes ALL rows (chunks) associated with a given physical file path.
    // Used on file-delete and before re-embedding a modified file (upsert).

    pub async fn delete_by_path(&self, physical_path: &str) -> Result<()> {
        // LanceDB delete takes a SQL-style predicate string
        let predicate = format!(
            "physical_path = '{}'",
            // Escape any single-quotes in the path itself
            physical_path.replace('\'', "''")
        );
        self.table
            .delete(&predicate)
            .await
            .context("Failed to delete rows from LanceDB")?;
        Ok(())
    }

    // ─── Upsert (delete old chunks then re-insert) ────────────────────────────
    // Call this when a file is modified so stale vectors are purged first.

    pub async fn upsert_file(
        &self,
        file_id: &str,
        filename: &str,
        physical_path: &str,
        vector: Vec<f32>,
    ) -> Result<()> {
        // Only delete once per physical path (caller may call this per-chunk;
        // deletion is idempotent in LanceDB so it's safe to call multiple times).
        self.delete_by_path(physical_path).await?;
        self.insert_file(file_id, filename, physical_path, vector).await
    }

    // ─── Search ──────────────────────────────────────────────────────────────
    // Cosine similarity search. Returns up to `limit` unique physical files
    // (deduplicated from chunk-level hits). `overfetch` controls how many raw
    // rows are pulled to satisfy deduplication needs.

    pub async fn search(
        &self,
        query_vector: Vec<f32>,
        limit: usize,
        overfetch: usize,
    ) -> Result<Vec<(String, String)>> {
        let over_limit = limit * overfetch;

        let mut stream = self
            .table
            .search(&query_vector)
            .limit(over_limit)
            .execute()
            .await
            .context("LanceDB search failed")?;

        let mut unique_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut results: Vec<(String, String)> = Vec::new();

        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read search batch")?;

            let filename_col = batch
                .column_by_name("filename")
                .context("Missing 'filename' column in result")?;
            let path_col = batch
                .column_by_name("physical_path")
                .context("Missing 'physical_path' column in result")?;

            let filenames = filename_col
                .as_any()
                .downcast_ref::<StringArray>()
                .context("'filename' column is not StringArray")?;
            let paths = path_col
                .as_any()
                .downcast_ref::<StringArray>()
                .context("'physical_path' column is not StringArray")?;

            for i in 0..batch.num_rows() {
                let phys = paths.value(i).to_string();
                if unique_paths.insert(phys.clone()) {
                    results.push((filenames.value(i).to_string(), phys));
                    if results.len() >= limit {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }
}
