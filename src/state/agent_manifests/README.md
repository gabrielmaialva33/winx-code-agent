# Agent detection manifests

Data-driven turn/state detection rules for interactive coding-agent TUIs
(claude, codex, gemini, cursor, opencode, ...), consumed by
`crate::state::turn_manifest`.

These TOML manifests are vendored from the [herdr](https://github.com/herdrdev/herdr)
project (`src/detect/manifests/`), commit `0cbd1a5aa847ab767334938e3bc858c68e613d70`
(v0.8.2 line), and are licensed under the Apache License 2.0 — see the
`LICENSE` file in this directory. They are redistributed here unmodified;
winx's ported rule engine maps herdr's states onto winx's `TurnState`
(`idle` → `awaiting_input`, `working` → `busy`, `blocked` → `awaiting_approval`,
`unknown` → `unknown`).

To refresh a manifest, copy the newer file from upstream herdr and rerun the
`turn_manifest` test suite (`cargo test turn_manifest`), which validates and
compiles every bundled manifest.

At runtime a manifest can be overridden without rebuilding by pointing
`WINX_AGENT_MANIFEST_DIR` at a directory containing `<id>.toml` files.
