# Codex Med model compatibility

Codex Med is released independently from upstream Codex. It does not copy or share login files:
the npm launcher defaults `CODEX_HOME` to `~/.codex-med`, or to `CODEX_MED_HOME` when explicitly
configured.

New GPT aliases and model capabilities arrive through the authenticated `/models` catalog. The
client therefore treats model-defined reasoning values as forward-compatible strings, decodes
catalog entries independently, and keeps a provider-scoped last-known-good cache for offline or
transient-failure recovery. Cache writes use an atomic replacement so an interrupted update cannot
truncate the active catalog.

The release embeds two identities:

- `codex --version` is the independent Codex Med version.
- `codex debug model-compatibility` prints the audited upstream revision, upstream npm release,
  protocol client version, cache schema, and tracked model-protocol file hashes.

OpenAI capability negotiation uses only the audited protocol client version: the `/models`
`client_version` query parameter, the HTTP `version` header, and the Codex User-Agent all advance
together. The Med package version remains independent and continues to identify local releases,
telemetry, state, and UI. This separation prevents a Med release number such as `0.1.7` from being
mistaken for an obsolete official Codex protocol version.

The compatibility manifest is
`codex-rs/build-info/upstream_compatibility.json`. Pull requests validate it against the pinned
upstream revision. A weekday scheduled workflow compares the same paths with the latest upstream
`main` and fails visibly when manual review is needed.

To adopt an upstream model update:

1. Fetch official `main` without merging it wholesale.
2. Review every tracked-file diff and port only the model protocol, endpoint, cache, and picker
   behavior that applies to Codex Med.
3. Run `python3 scripts/check_model_compatibility.py --upstream-ref <reviewed-ref>` while iterating.
4. Update the manifest revision, upstream npm release, protocol client version, and SHA-256 values
   only after review.
5. Run the protocol, API, models-manager, model-provider, TUI, CLI, launcher, and real authenticated
   model-list/turn E2E suites before releasing a new Codex Med version.

This is a selective compatibility process, not an automatic merge. It preserves the medical fork's
independent features and credential boundary while still detecting upstream model evolution quickly.
