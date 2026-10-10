# WebAssembly grammars for the tests

Built with tree-sitter CLI 0.27.1 (`tree-sitter build --wasm`) from the grammars' published crates
(MIT): `tree-sitter-json.wasm` — tree-sitter-json 0.24.8 (no external scanner),
`tree-sitter-toml.wasm` — tree-sitter-toml-ng 0.7.0 (with an external scanner). Small on purpose: the
catalog's plugins ship the real ones.
