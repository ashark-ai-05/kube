# Native local NLP

The optional AI edition bundles **Qwen3.5-2B Q4_K_M**, a pretrained model converted to GGUF by Unsloth, and the native llama.cpp CPU runtime. No training or fine-tuning is performed. Model bytes: **1,280,835,840** (1.28 GB / 1.19 GiB), plus the runtime. The manifest pins the source revision and SHA-256 of every downloaded artifact. No weights or runtime binaries are added to ordinary Git history.

## Enable

For a packaged AI release, extract the entire archive and run `./kube` with the adjacent `ai/` directory intact. Press **Ctrl-Space**, click **Ask Kube**, or use **:ask**. The drawer title says **local model available** when discovery succeeds.

For a source build on Apple Silicon:

```sh
cargo build --release --locked
python3 scripts/package-ai.py target/release --platform macos-arm64
./target/release/kube --kubeconfig /path/to/clusters.yaml
```

Use `--platform ubuntu-x64` on Linux x86-64. Packaging downloads assets once, checks their hashes and creates `target/release/ai`. It refuses to overwrite an existing bundle. Replace an older FunctionGemma bundle with a newly assembled one; do not combine old weights with the new prompt. Alternatively, set `KUBE_AI_DIR=/absolute/path/to/ai` before starting the app. Restart after installing/upgrading. No Python, Ollama, cloud endpoint or separately configured server is required at application runtime.

## Interaction

Common phrases use the instant built-in parser. Other wording invokes the native model. Try:

- “Could you bring up unhealthy pods in orders?”
- “I want the manifest of the selected deployment”
- “Show me CPU and memory usage for this pod”
- “Change context to Team-East”

The drawer displays the current cluster, namespace, selected resource and proposed action. Enter applies the preview; typing invalidates it; Esc cancels. Scope changes invalidate pending work. It supports listing six resource kinds, namespace/context selection, and selected-resource overview, YAML, events, logs, previous logs, metrics and relationships. It does not generate diagnoses, execute shell commands or mutate the cluster. Select a pod first before requesting its inspection or logs. Arbitrary named targets, list thresholds, sorting, exclusions and compound requests are outside the model action vocabulary; use the existing table controls for those operations.

## Runtime and validation

Only the typed query and fixed action specification reach the worker. No kubeconfig, credentials, pod contents or logs are sent. The worker uses authenticated loopback HTTP, two CPU threads, no GPU offloading, a 4,096-token context, bounded JSON output, and a 90-second overall deadline. It starts on demand and exits after every request or cancellation; idle model memory is not retained. Local cold requests took roughly 8–13 seconds during evaluation; a hosted Mac needed 31 seconds for its first runtime startup alone, so slower hosts can take substantially longer. Inference runs outside the UI loop and Esc cancels immediately.

A constrained JSON grammar produces the closed Rust action type. Validation rejects unknown/duplicate arguments, invented scopes, missing explicit namespaces, unsupported conditions, named inspection targets and compound actions. These checks reduce mistakes; they do not prove semantic correctness. Model proposals remain **experimental** and always require a visible preview. No claim of broad or production-level NLP accuracy is made.

## Evaluation

The evaluation record in [evaluations/qwen-2026-09-28](evaluations/qwen-2026-09-28/README.md) distinguishes raw model output from built-in parsing and validated proposals. An initial fresh 45-case trial scored **34/45** raw; after expanding the fixed action specification, the same cases scored **37/45** raw and became a regression set, not a held-out test. Rejections and wrong actions are recorded, including failures on scope and previous-instance semantics. The final combined built-in/native/validation path passed **45/45 regression cases**, including the 20 unsupported requests; this is not a raw-model score. A schema-order regression found during native integration is covered by a unit test. Do not interpret successful schema validation as successful intent understanding.

```sh
# Standard built-in vocabulary, works without model assets:
cargo run --locked --example assistant-eval
# Actual cold native-model path with built-ins and output validation:
KUBE_AI_DIR=/path/to/ai cargo run --release --locked --example assistant-eval -- --cases ai/eval-paraphrases.json
# Force raw inference plus product validation, bypassing built-in interpretation:
KUBE_AI_DIR=/path/to/ai cargo run --release --locked --example assistant-eval -- --model-only
# Optional native-runtime integration:
KUBE_AI_DIR=/path/to/ai cargo test --locked --test assistant_local -- --ignored
```

The forced-model command is diagnostic and can fail on requests that the built-in path handles reliably. Historical FunctionGemma and tiny-model trials remain under `evaluations/`.

## ONNX and size

[ONNX Runtime](https://onnxruntime.ai/docs/genai/) can run pretrained local models without training. ONNX is a model format/runtime choice, not an accuracy guarantee. This release uses the evaluated [Qwen pretrained model](https://huggingface.co/Qwen/Qwen3.5-2B) through native GGUF because it fits the optional download budget and the existing CPU packaging path. The 0.8B Qwen and 1.2B LFM trials below 700 MB missed too many constraints in this task. No ONNX runtime is included in this release.
