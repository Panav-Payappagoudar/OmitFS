use anyhow::{Context, Result};
use candle_core::{Device, Tensor, IndexOp, DType};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;

pub struct EmbeddingEngine {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl EmbeddingEngine {
    /// Downloads (first run) or loads from local HuggingFace cache.
    /// All inference is 100% local — no API key, no network after first run.
    pub fn new() -> Result<Self> {
        let device = Device::Cpu;

        // hf-hub caches to ~/.cache/huggingface/hub automatically
        let api = Api::new().context("Failed to init HuggingFace cache API")?;
        let repo = api.repo(Repo::new(
            "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            RepoType::Model,
        ));

        let config_path   = repo.get("config.json").context("Failed to fetch config.json")?;
        let tokenizer_path= repo.get("tokenizer.json").context("Failed to fetch tokenizer.json")?;
        let weights_path  = repo.get("model.safetensors").context("Failed to fetch model.safetensors")?;

        // Parse BERT config
        let config_str = std::fs::read_to_string(&config_path)?;
        let config: Config = serde_json::from_str(&config_str)
            .context("Failed to parse BERT config.json")?;

        // Load tokenizer
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Tokenizer load error: {}", e))?;

        // Memory-map safetensors weights into VarBuilder
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[&weights_path],
                DType::F32,
                &device,
            )?
        };
        let model = BertModel::load(vb, &config)
            .context("Failed to load BertModel")?;

        Ok(Self { model, tokenizer, device })
    }

    /// Embed a text string into a 384-dimensional f32 vector using CLS pooling.
    /// Takes &mut self because candle's Tensor ops may update internal state.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        // Encode and cap at 512 tokens (BERT hard limit)
        let encoding = self.tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenizer encode error: {}", e))?;

        let mut ids: Vec<u32> = encoding.get_ids().to_vec();
        ids.truncate(512); // safety cap

        if ids.is_empty() {
            return Ok(vec![0.0f32; 384]);
        }

        let token_ids      = Tensor::new(ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let token_type_ids = token_ids.zeros_like()?;

        let embeddings = self.model
            .forward(&token_ids, &token_type_ids)
            .context("BERT forward pass failed")?;

        // CLS token = position 0
        let cls: Vec<f32> = embeddings.i((0, 0, ..))?.to_vec1()?;
        Ok(cls)
    }
}
