use anyhow::{Context, Result};
use candle_core::{Device, DType, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;
use tracing::info;

pub struct EmbeddingEngine {
    model:     BertModel,
    tokenizer: Tokenizer,
    device:    Device,
}

impl EmbeddingEngine {
    /// Loads from ~/.omitfs_data/model if present, otherwise downloads weights
    /// from HuggingFace cache (requires internet for first run).
    /// Auto-detects CUDA → Metal → CPU in that priority order.
    pub fn new(data_dir: &std::path::Path) -> Result<Self> {
        let device = Self::best_device();
        info!("Embedding device: {:?}", device);

        let model_dir = data_dir.join("model");
        let (config_path, tokenizer_path, weights_path) = if model_dir.join("config.json").exists() {
            info!("Loading model from local path: {:?}", model_dir);
            (
                model_dir.join("config.json"),
                model_dir.join("tokenizer.json"),
                model_dir.join("model.safetensors"),
            )
        } else {
            let api  = Api::new().context("Failed to init HuggingFace cache API")?;
            let repo = api.repo(Repo::new(
                "sentence-transformers/all-MiniLM-L6-v2".to_string(),
                RepoType::Model,
            ));
            (
                repo.get("config.json").context("Fetch config.json")?,
                repo.get("tokenizer.json").context("Fetch tokenizer.json")?,
                repo.get("model.safetensors").context("Fetch model.safetensors")?,
            )
        };

        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(&config_path)?
        ).context("Parse BERT config.json")?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Tokenizer load: {e}"))?;

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, &device)?
        };
        let model = BertModel::load(vb, &config).context("Load BertModel")?;

        Ok(Self { model, tokenizer, device })
    }

    /// Pick the best available compute device.
    fn best_device() -> Device {
        // Try CUDA first
        #[cfg(feature = "cuda")]
        if let Ok(d) = Device::new_cuda(0) {
            return d;
        }
        // Try Apple Metal
        #[cfg(feature = "metal")]
        if let Ok(d) = Device::new_metal(0) {
            return d;
        }
        Device::Cpu
    }

    /// Embed text into a 384-dim f32 vector via CLS pooling.
    /// Input is capped at 512 BERT tokens; empty input returns a zero vector.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let encoding = self.tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenizer encode: {e}"))?;

        let mut ids: Vec<u32> = encoding.get_ids().to_vec();
        ids.truncate(512);

        if ids.is_empty() {
            return Ok(vec![0.0_f32; 384]);
        }

        let token_ids      = Tensor::new(ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let token_type_ids = token_ids.zeros_like()?;

        let embeddings = self.model
            .forward(&token_ids, &token_type_ids)
            .context("BERT forward pass")?;

        // CLS token at position 0
        let cls: Vec<f32> = embeddings.i((0, 0, ..))?.to_vec1()?;
        Ok(cls)
    }
}
