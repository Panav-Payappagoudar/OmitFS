use anyhow::{Context, Result};
use arrow_array::{
    Float32Array, FixedSizeListArray, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::StreamExt;
use lancedb::query::ExecutableQuery;
use std::path::PathBuf;
use std::sync::Arc;

const VEC_DIM: i32   = 384;
// v2 adds chunk_text column — use a separate table name for clean migration
const TABLE: &str    = "files_v2";

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

        let schema = Arc::new(Self::schema());

        let table = match conn.open_table(TABLE).execute().await {
            Ok(t) => t,
            Err(_) => {
                let empty = RecordBatchIterator::new(
                    std::iter::empty::<Result<RecordBatch, arrow_schema::ArrowError>>(),
                    schema.clone(),
                );
                conn.create_table(TABLE, Box::new(empty))
                    .execute()
                    .await
                    .context("Failed to create LanceDB table")?
            }
        };

        Ok(Self { table })
    }

    fn schema() -> Schema {
        Schema::new(vec![
            Field::new("file_id",       DataType::Utf8, false),
            Field::new("filename",      DataType::Utf8, false),
            Field::new("physical_path", DataType::Utf8, false),
            Field::new("chunk_text",    DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    VEC_DIM,
                ),
                false,
            ),
        ])
    }

    // ─── Insert ──────────────────────────────────────────────────────────────

    pub async fn insert_file(
        &self,
        file_id:       &str,
        filename:      &str,
        physical_path: &str,
        chunk_text:    &str,
        vector:        Vec<f32>,
    ) -> Result<()> {
        let schema = Arc::new(Self::schema());

        let ids    = Arc::new(StringArray::from(vec![file_id]));
        let names  = Arc::new(StringArray::from(vec![filename]));
        let paths  = Arc::new(StringArray::from(vec![physical_path]));
        let chunks = Arc::new(StringArray::from(vec![chunk_text]));

        let flat       = Float32Array::from(vector);
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vecs       = Arc::new(FixedSizeListArray::try_new(
            list_field, VEC_DIM, Arc::new(flat), None,
        ).context("Failed to build FixedSizeListArray")?);

        let batch = RecordBatch::try_new(schema.clone(), vec![ids, names, paths, chunks, vecs])
            .context("Failed to build RecordBatch")?;

        let reader = RecordBatchIterator::new(
            vec![Ok(batch)].into_iter(),
            schema,
        );

        self.table.add(Box::new(reader)).execute().await
            .context("Failed to insert into LanceDB")?;

        Ok(())
    }

    // ─── Delete by physical path ──────────────────────────────────────────────

    pub async fn delete_by_path(&self, physical_path: &str) -> Result<()> {
        let predicate = format!(
            "physical_path = '{}'",
            physical_path.replace('\'', "''")
        );
        self.table.delete(&predicate).await
            .context("Failed to delete from LanceDB")?;
        Ok(())
    }

    // ─── Search → (filename, path) ───────────────────────────────────────────

    pub async fn search(
        &self,
        query_vector: Vec<f32>,
        limit:        usize,
        overfetch:    usize,
    ) -> Result<Vec<(String, String)>> {
        let rows = self.search_with_chunks(query_vector, limit, overfetch).await?;
        Ok(rows.into_iter().map(|(f, p, _)| (f, p)).collect())
    }

    // ─── Search → (filename, path, chunk_text) — used by RAG & re-ranker ─────

    pub async fn search_with_chunks(
        &self,
        query_vector: Vec<f32>,
        limit:        usize,
        overfetch:    usize,
    ) -> Result<Vec<(String, String, String)>> {
        let over_limit = limit * overfetch;

        let mut stream = self.table
            .search(&query_vector)
            .limit(over_limit)
            .execute()
            .await
            .context("LanceDB search failed")?;

        let mut seen:    std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut results: Vec<(String, String, String)>     = Vec::new();

        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read search batch")?;

            let names  = col_str(&batch, "filename")?;
            let paths  = col_str(&batch, "physical_path")?;
            let chunks = col_str(&batch, "chunk_text")?;

            for i in 0..batch.num_rows() {
                let phys  = paths.value(i).to_string();
                let chunk = chunks.value(i).to_string();
                if seen.insert(phys.clone()) {
                    results.push((names.value(i).to_string(), phys, chunk));
                    if results.len() >= limit { return Ok(results); }
                }
            }
        }

        Ok(results)
    }
}

// ─── Helper ───────────────────────────────────────────────────────────────────

fn col_str<'a>(
    batch: &'a arrow_array::RecordBatch,
    name:  &str,
) -> Result<&'a StringArray> {
    batch.column_by_name(name)
        .with_context(|| format!("Missing '{name}' column"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("Column '{name}' is not StringArray"))
}
