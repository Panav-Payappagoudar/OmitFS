use anyhow::{Context, Result};
use arrow_array::{RecordBatch, RecordBatchIterator, StringArray, Float32Array, FixedSizeListArray};
use arrow_schema::{DataType, Field, Schema};
use lancedb::Table;
use std::sync::Arc;
use std::path::PathBuf;
use futures::stream::StreamExt;

pub struct OmitDb {
    table: Table,
}

impl OmitDb {
    pub async fn init(db_path: PathBuf) -> Result<Self> {
        let uri = db_path.to_str().context("Invalid db path")?;
        let conn = lancedb::connect(uri).execute().await?;
        
        let schema = Arc::new(Schema::new(vec![
            Field::new("file_id", DataType::Utf8, false),
            Field::new("filename", DataType::Utf8, false),
            Field::new("physical_path", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), 384),
                false,
            ),
        ]));

        let table = match conn.open_table("files").execute().await {
            Ok(t) => t,
            Err(_) => {
                let empty_batches = RecordBatchIterator::new(vec![], schema.clone());
                conn.create_table("files", empty_batches).execute().await?
            }
        };

        Ok(Self { table })
    }

    pub async fn insert_file(&self, file_id: &str, filename: &str, physical_path: &str, vector: Vec<f32>) -> Result<()> {
        let file_id_array = StringArray::from(vec![file_id]);
        let filename_array = StringArray::from(vec![filename]);
        let path_array = StringArray::from(vec![physical_path]);
        
        let values = Float32Array::from(vector);
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = FixedSizeListArray::try_new_from_values(list_field, 384, Arc::new(values), None)?;

        let schema = self.table.schema().await?;
        
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(file_id_array),
                Arc::new(filename_array),
                Arc::new(path_array),
                Arc::new(vector_array),
            ],
        )?;

        self.table.add(vec![batch]).execute().await?;
        Ok(())
    }

    pub async fn search(&self, query_vector: Vec<f32>, limit: usize) -> Result<Vec<(String, String)>> {
        // Query more vectors internally to ensure we have enough after deduplication of chunks
        let mut results = self.table
            .search(&query_vector)
            .limit(limit * 5)
            .execute()
            .await?;
        
        let mut unique_paths = std::collections::HashSet::new();
        let mut found_files = Vec::new();
        
        while let Some(batch) = results.next().await {
            let batch = batch?;
            let filename_col = batch.column_by_name("filename").context("Missing filename")?;
            let path_col = batch.column_by_name("physical_path").context("Missing physical_path")?;
            
            let filenames = filename_col.as_any().downcast_ref::<StringArray>().context("Invalid filename type")?;
            let paths = path_col.as_any().downcast_ref::<StringArray>().context("Invalid path type")?;
            
            for i in 0..batch.num_rows() {
                let physical_path = paths.value(i).to_string();
                if !unique_paths.contains(&physical_path) {
                    unique_paths.insert(physical_path.clone());
                    found_files.push((
                        filenames.value(i).to_string(),
                        physical_path
                    ));
                    
                    if found_files.len() >= limit {
                        return Ok(found_files);
                    }
                }
            }
        }
        
        Ok(found_files)
    }
}
