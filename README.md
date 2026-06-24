<p align="center"><strong>codex-med</strong> — a medical-domain coding agent built on top of OpenAI's Codex CLI.</p>

<p align="center">
  Adds biomedical tools (UniProt / GenBank / PDB lookup, antibody training database queries, planned external model gateway) on top of the upstream Codex CLI.
</p>

---

## Quickstart

### Install via npm (Linux x64)

```shell
npm install -g @gair/codex-med
```

Currently only `linux-x64` (glibc) prebuilt binaries are published. Other platforms can build from source — see [Building from source](#building-from-source).

Then run:

```shell
codex-med
```

### Sign in

Inside the TUI, follow the prompts to sign in with your ChatGPT account or paste an API key. See `docs/config.md` for full configuration reference.

---

## What's different from upstream Codex

This is a fork; everything in the upstream README about agents, sandboxing, MCP support, etc. still applies. The medical-fork-specific additions:

- **Built-in biomedical lookup tools** — fetch UniProt entries, GenBank records, PDB structures, and search UniProt directly from the agent loop
- **Antibody training database tool** — read-only SQL queries over a local SQLite database (`training_ready_v1.sqlite`); see `docs/` for schema
- **External model gateway integration** *(planned)* — see [docs/model-integration.md](docs/model-integration.md) for the design and onboarding guide for connecting self-hosted models (affinity prediction, structure prediction, ADMET, etc.) via MCP

---

## Building from source

Requires Rust 1.95+ and Node 16+.

```shell
git clone https://github.com/openai/codex.git codex-med
cd codex-med/codex-rs
cargo build --release -p codex-cli --bin codex
```

The binary lands at `codex-rs/target/release/codex`. To package it as an npm tarball locally, see `codex-cli/scripts/build_npm_package.py` and `scripts/stage_npm_packages.py`.

---

## Docs

- [Model integration guide](docs/model-integration.md) — how to plug self-hosted prediction models into the agent
- [Configuration](docs/config.md)
- [Contributing](docs/contributing.md)

---

This repository is licensed under the [Apache-2.0 License](LICENSE).
