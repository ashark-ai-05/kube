# Pretrained CPU model investigation — 2026-09-28

No training or fine-tuning was performed. This is a small development evaluation, not evidence of general Kubernetes expertise.

## Candidates

- Qwen3.5-0.8B Q4_K_M: 532,517,120 bytes, Unsloth revision `6ab461498e2023f6e3c1baea90a8f0fe38ab64d0`, SHA-256 `bd258782e35f7f458f8aced1adc053e6e92e89bc735ba3be89d38a06121dc517`. Constrained output plus a 512-token reasoning trial still dropped filters/scopes. Not shipped.
- LiquidAI LFM2.5-1.2B Instruct QAD Q4_0: 695,755,488 bytes, official revision `8ed288026e23958ad9dfa92d53ed773a8eee7125`, SHA-256 `bb741ebb106d543e9de114b843a3d3d73d51c74b5801e69da2abde821a0cb3e1`. Missed scope, view and rejection conditions in this prompt. Not shipped. The result does not establish the model's quality for other applications or prompts.
- Qwen3.5-2B Q4_K_M: 1,280,835,840 bytes, pinned source/quantization/checksum in `../../manifest.json`. Selected for the optional bundle, with explicit validation and previews.

## Raw model results

The initial 45 fresh cases include 25 navigation requests and 20 unsupported requests. `heldout-results.json` scored **34/45** raw. Expanding the fixed action specification produced **37/45** on the same cases (`regression-results.json`). At that point these became development/regression cases, not held-out evidence. Raw failures included omitted namespaces, current-versus-previous logs, and silently dropped conditions. The product's typed validation and built-in phrases are separate from those raw scores.

The Python evaluator keeps one native worker warm, uses CPU only, two threads, 4,096 context tokens, temperature zero, no thinking, 220 output tokens and the JSON schema. Cold Rust product evaluation starts/stops a worker for each model request and includes more validation, so latency and accepted-action scores differ. The final cold product results are recorded below.

```sh
python3 ai/evaluations/qwen-2026-09-28/evaluate.py --bundle /path/to/ai --output /tmp/raw-results.json
# Initial prompt, before the expanded action specification:
python3 ai/evaluations/qwen-2026-09-28/evaluate.py --bundle /path/to/ai --prompt ai/evaluations/qwen-2026-09-28/initial-prompt.txt --output /tmp/initial-results.json
```

The accepted read/navigation vocabulary is deliberately limited. The standard binary remains usable without the model; accepted model suggestions require a scoped preview. Nothing in this evaluation authorizes writes or automatic diagnostic claims.

## Final product acceptance

The final built-in + native inference + validation pipeline passed **45/45 regression cases**, including all 20 unsupported requests; 39 cases invoked the actual local model. `product-results.txt` contains every result. These are development cases, not a claim of 100% general language accuracy. Raw model accuracy remains the separate 37/45 result above. Built-ins handle known phrases; validators reject unsupported or altered requests instead of silently executing a partial interpretation.

Cold native interpretation on the development Apple Silicon host ranged from 8.11 to 12.72 seconds in this run, with other local tests running concurrently. Each query loads a fresh two-thread CPU worker. Warm raw timings cannot be substituted for these end-to-end times. The terminal smoke test verified an accepted model-generated YAML action and Escape cancellation within one second. No Kubernetes data was sent into the model.
