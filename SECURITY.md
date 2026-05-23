# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.4.x   | ✅ Yes    |
| < 0.4   | ❌ No     |

## Reporting a Vulnerability

OmitFS is a **local-first, air-gapped** application. All data stays on your machine.

If you discover a security vulnerability, please **do not** open a public GitHub issue.
Instead, report it privately via GitHub's [Security Advisories](../../security/advisories/new) feature.

We aim to respond within **72 hours** and will provide a fix within **14 days** for confirmed vulnerabilities.

## Security Architecture

- **Encryption at rest**: AES-256-GCM (opt-in via `encryption_enabled = true` in `config.toml`)
- **Key management**: 32-byte random key stored at `~/.omitfs_data/encryption.key` (Unix: mode 0600)
- **No telemetry**: Zero network calls after initial model download
- **No API keys**: Fully local inference via Candle + Ollama
- **Web UI**: Served on `127.0.0.1` only — not exposed to the network by default

## Known Limitations

- The web server (`omitfs serve`) binds to `127.0.0.1` only and should not be exposed to untrusted networks
- `encryption.key` must be backed up separately — losing it means losing access to indexed content (raw files are unaffected)
- Tesseract and Whisper integrations spawn external processes; ensure those binaries are from trusted sources
