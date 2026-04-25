<div align="center">

# 🌌 OmitFS
**Intent-Driven Semantic File Routing for the Modern Operating System**

[![Rust](https://img.shields.io/badge/Rust-Bare_Metal-blue?style=for-the-badge&logo=rust)](https://www.rust-lang.org)
[![FUSE](https://img.shields.io/badge/FUSE-Kernel_Bridge-darkgreen?style=for-the-badge&logo=linux)](https://github.com/libfuse/libfuse)
[![LanceDB](https://img.shields.io/badge/LanceDB-Vector_Storage-orange?style=for-the-badge)](https://lancedb.com/)
[![Candle](https://img.shields.io/badge/Candle-Local_SLM-red?style=for-the-badge)](https://github.com/huggingface/candle)

*OmitFS obliterates the 50-year-old paradigm of hierarchical directory trees. It replaces rigid folders with a high-dimensional, LLM-powered latent space directly inside your OS kernel.*

</div>

---

## 📖 The Narrative: Why OmitFS Exists

**The year is 1964.** The hierarchical directory tree is introduced in the MULTICS operating system. It was a brilliant solution for organizing small clusters of files. 

**The year is 2026.** We are still using the exact same structure. We force human brains to memorize arbitrary file paths, obscure folder names, and rigid organizational systems just to find a single document. *The hierarchy is broken.*

**Enter OmitFS.** 
OmitFS is a paradigm-shifting, zero-dependency semantic file system. It abandons rigid paths entirely. Instead of forcing you to memorize where you put a file, **you simply tell the operating system what you want.** The physical hard drive acts as a flat, hidden void, and directories are hallucinated in real-time based purely on your semantic intent.

---

## ⚙️ The Core Mechanics

1. **The Hidden Void**: Standard directories no longer exist. All files are ingested into a single, flat, hidden physical directory. 
2. **Semantic Embedding**: As a file is dropped into the void, its contents are read and converted into 384-dimensional mathematical vectors using a local Small Language Model. These vectors represent the *meaning* of the file.
3. **Dynamic Hallucination**: When you type a command like `cd "decentralized web3 auth architecture"`, OmitFS intercepts the kernel call, mathematically embeds your prompt, and instantly generates a virtual folder in RAM containing the exact relevant files.
4. **Ephemeral Existence**: The folder only exists for as long as you are standing inside it. Once you leave, the structure dissolves back into the void.

---

## 🏗️ Technical Architecture & Constraints

OmitFS is not a fragile Python wrapper or a bloated Electron app. It is a production-grade infrastructure tool that operates beneath the application layer, interfacing directly with the OS kernel. It adheres to absolute brutalist engineering principles:

- 🦀 **Language**: Pure Rust. Chosen for bare-metal speed, sub-millisecond execution, and uncompromising memory safety.
- 🌉 **The OS Bridge (`fuser`)**: A custom Filesystem in Userspace (FUSE) driver that seamlessly intercepts standard POSIX system calls (`cd`, `ls`, `cat`, `vim`) directly from the Linux/macOS kernel.
- 🧠 **The Inference Engine (`candle`)**: Runs a quantized Hugging Face SLM (`all-MiniLM-L6-v2`) 100% locally on the CPU. It translates text payloads into mathematical vectors with zero network latency.
- 🗄️ **The Vector Database (`LanceDB`)**: An embedded vector search engine running natively inside the Rust process to execute hyper-fast cosine similarity searches.
- 🛡️ **Absolute Privacy (Air-Gapped)**: Zero external dependencies. No background Docker containers. No Python environments. No data is ever sent to OpenAI or the cloud. **100% offline.**
- 📁 **Flawless POSIX Compliance**: The virtual files generated feed accurate byte-sizes, permissions (`-rw-r--r--`), and modification timestamps back to the OS. Standard tools like `grep` and `cat` work flawlessly.

---

## ⚡ Quick Start & Usage

The interface is pure, brutalist terminal interaction. There are no web dashboards or graphical overlays.

### 1. Prerequisites
You will need Rust and a FUSE-compatible OS (Linux or macOS / WSL2 for Windows).
```bash
# Install FUSE dependencies (Ubuntu/Debian)
sudo apt install libfuse-dev
```

### 2. Compile the System
Clone the repository and compile the hyper-fast release binary:
```bash
git clone https://github.com/Panav-Payappagoudar/OmitFS.git
cd OmitFS
cargo build --release
```

### 3. Initialize & Mount the Void
Initialize the system, start the background SLM ingestion daemon, and mount the file system to your OS:
```bash
# Initialize the hidden void and Vector DB
./target/release/omitfs init

# Start the background daemon (Run in a separate terminal)
./target/release/omitfs daemon

# Mount the FUSE driver
mkdir -p ~/OmitFS_Mount
./target/release/omitfs mount ~/OmitFS_Mount
```

### 4. Navigate by Meaning
Drop raw text/code files into `~/.omitfs_data/raw`. Then, navigate your computer using raw human intent:

```bash
# The OS queries your semantic intent
cd "~/OmitFS_Mount/tax documents from 2024"

# The terminal reveals the exact relevant files
ls -la

# Interact with the virtual file using standard POSIX tools
cat 2024_W2_form.md
vim Q3_expenses.txt
```

---

<div align="center">
<i>Built for the future of the Operating System.</i>
</div>
