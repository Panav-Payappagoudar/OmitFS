<div align="center">

# 🧠 OmitFS
**Semantic File Routing for the Modern Operating System**

[![Rust](https://img.shields.io/badge/rust-1.75%2B-blue.svg?style=for-the-badge&logo=rust)](https://www.rust-lang.org)
[![FUSE](https://img.shields.io/badge/fuse-kernel-darkgreen.svg?style=for-the-badge&logo=linux)](https://github.com/libfuse/libfuse)
[![LanceDB](https://img.shields.io/badge/LanceDB-Vector-orange.svg?style=for-the-badge)](https://lancedb.com/)
[![Candle](https://img.shields.io/badge/Candle-ML-red.svg?style=for-the-badge)](https://github.com/huggingface/candle)

OmitFS is a high-performance, zero-dependency semantic file system built in **bare-metal Rust**. It annihilates the traditional hierarchical directory tree, replacing it with a dynamic, LLM-powered latent space. Navigate your local files by **intent** rather than rigid paths.

</div>

---

## 🚀 How It Works

1. **The Hidden Void:** Standard directories are obsolete. Your files are ingested into a flat, hidden storage layer (`~/.omitfs_data/raw`).
2. **Local SLM Embedding:** As files are added, an embedded Small Language Model (`all-MiniLM-L6-v2`) instantly tokenizes and mathematically embeds the content into 384-dimensional vectors via Hugging Face's Rust-native **Candle** engine. Zero network calls. Zero OpenAI APIs. 100% private.
3. **Dynamic Hallucination:** When you execute a standard terminal command (e.g., `cd "tax documents from 2024"`), the FUSE kernel driver intercepts the POSIX call. The query is instantly vectorized and matched against the embedded **LanceDB** vector store using cosine similarity.
4. **Virtual Materialization:** OmitFS instantaneously hallucinates a virtual directory in RAM containing the exact files you meant to look for, materialized as real POSIX inodes.

---

## 🛠️ Tech Stack & Architecture

- **Core Language:** Rust 2021 Edition
- **Kernel Bridge:** `fuser` (Filesystem in Userspace)
- **AI Engine:** `candle-core` & `candle-transformers` (Local ML inference)
- **Vector Database:** Embedded `LanceDB` with `arrow-array`
- **File Ingestion:** `notify` (Asynchronous recursive background watching)
- **Asynchronous Runtime:** `tokio`
- **CLI & Diagnostics:** `clap`, `tracing`, `anyhow`

---

## ⚡ Quick Start

### 1. Prerequisites
You will need Rust and a FUSE-compatible OS (Linux/macOS). 
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install FUSE dependencies (Ubuntu/Debian)
sudo apt install libfuse-dev
```

### 2. Build from Source
Clone the repository and compile the highly optimized release binary:
```bash
git clone https://github.com/Panav-Payappagoudar/OmitFS.git
cd OmitFS
cargo build --release
```

### 3. Initialize & Start the Daemon
Initialize the hidden storage layer and local Vector DB, then start the background ingestion pipeline:
```bash
# Initialize database and model weights
./target/release/omitfs init

# Start the background daemon (Leave this running)
./target/release/omitfs daemon
```

### 4. Mount the Matrix
In a new terminal, create a mount point and attach OmitFS:
```bash
mkdir -p ~/OmitFS_Mount
./target/release/omitfs mount ~/OmitFS_Mount
```

### 5. Navigate by Meaning
Drop raw text/code files into `~/.omitfs_data/raw`. Then simply `cd` into concepts:
```bash
# Instead of traversing folders, type your intent:
cd "~/OmitFS_Mount/project architecture and deep learning notes"

# The OS sees a valid directory. The files inside are exactly what you asked for.
ls -la
```

---

## 🧠 System Constraints & Safety

- **100% Local Execution:** Model weights are pulled directly from Hugging Face once and cached locally. No telemetry, no external database pings.
- **Graceful Degradation:** If the vector search fails or yields low confidence, OmitFS elegantly falls back to standard POSIX `ENOENT` (No such file or directory) to prevent kernel panics.
- **Zero-Dependency:** The final compiled binary contains everything needed to run: the database, the machine learning inference engine, and the filesystem driver.

---

## 📜 License
MIT License. Built for the modern OS.
