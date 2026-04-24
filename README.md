# OmitFS

**Semantic routing for the modern operating system.**

OmitFS is a high-performance, zero-dependency file system (FUSE) that replaces the traditional hierarchical directory tree with a dynamic latent space. It allows you to navigate your local files by **intent** rather than rigid paths.

## How it works
1. **The Hidden Void:** Your files are stored in a flat, hidden ingestion layer.
2. **Semantic Embedding:** Files are indexed locally using a Rust-based Small Language Model (SLM).
3. **Dynamic Hallucination:** When you `cd` into a conceptual folder (e.g., `cd "tax documents 2024"`), OmitFS generates a virtual directory in real-time containing mathematically relevant files.

## Tech Stack
* **Language:** Rust
* **Kernel Bridge:** FUSE (`fuser` crate)
* **ML Engine:** `candle` (Local-first, no cloud APIs)
* **Database:** `LanceDB` (Embedded vector store)

## Quick Start
```bash
# Clone and build
git clone [https://github.com/Panav-Payappagoudar/OmitFS.git](https://github.com/Panav-Payappagoudar/OmitFS.git)
cd OmitFS && cargo build --release

# Mount the system
./target/release/omitfs mount ~/OmitFS

# Navigate by meaning
cd "~/OmitFS/project architecture and deep learning notes"
ls -la
