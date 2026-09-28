# Offline command model

The optional AI release contains **FunctionGemma 270M Q8_0** (291,557,792 bytes) and the pinned llama.cpp CPU runtime. The ordinary Git repository contains only source, the native tool prompt, evaluation fixtures, license notices and an artifact manifest. All assets are SHA-256 checked by `scripts/package-ai.py`; weights and runtimes are ignored by Git.

## Contract

`Ctrl-Space` opens Ask Kube. Common supported phrases use a deterministic interpreter. Other wording uses the bundled model when present. Both paths produce the same closed, read-only `Intent` type and a visible action preview bound to the current cluster/namespace and selected object. Enter applies; typing invalidates the preview; Esc cancels. Changing scope invalidates pending work. Unsupported tool names, extra arguments, prompt delimiters and invented namespace defaults are rejected. Writes and shell execution are absent from the action vocabulary.

Only the typed query and fixed action definitions are sent to the local worker. No resource contents, logs, kubeconfig credentials or conversation history are sent. The per-request worker uses authenticated loopback HTTP with bounded response bodies, two CPU threads, 2,048 context tokens and a 30-second outer timeout. It is owned by a cancellable task and killed on drop. It never downloads models at runtime. The app works without the bundle.

## Evaluation and limitations

Local validation on 2026-09-28:

- The **shipped common-phrase path** passed all 14 evaluation cases, including scope, previous logs, diagnosis navigation and mutation rejection.
- The actual packaged model successfully produced a validated previous-log action in the ignored integration test. The final CPU-only run completed in 1.06 seconds with measured peak worker RSS of 706.3 MiB; the worker exited afterward. Device, operation and KV-cache GPU offloading are explicitly disabled. These are local measurements, not a guarantee on other CPUs.
- The **raw unfine-tuned model** passed only 7/14 cases after output validation and the mutation guard (4/11 read/navigation cases). Accepted mistakes included interpreting “all namespaces” as the literal namespace `all`; other responses invented namespaces or malformed arguments. Raw evaluation deliberately exits nonzero. It is **not an accuracy acceptance pass**.
- A few-shot prompt trial did not improve the aggregate score and regressed previous-log selection; that trial is not shipped.

For that reason, model output is labelled **experimental**, never auto-applied, and is not used to generate diagnostic claims. The current release is useful for the supported built-in phrases; arbitrary natural language remains an experimental aid. Domain fine-tuning and a larger held-out paraphrase suite are required before removing that label. Model size is not evidence of accuracy, and grammar/schema validity is not evidence of correct interpretation.

Cold startup shares the full 30-second request deadline. An earlier eight-second startup cutoff failed on the macOS release runner even though local and Linux checks passed; that separate cutoff has been removed. Cancellation still drops and kills the worker immediately.

```sh
cargo run --locked --example assistant-eval
KUBE_AI_DIR=/path/to/package/ai cargo test --locked --test assistant_local -- --ignored
KUBE_AI_DIR=/path/to/package/ai cargo run --locked --example assistant-eval -- --model-only
```

The last command is diagnostic and currently fails as documented. `prompt.txt` follows the model's native declaration/call format; the generic OpenAI tool-call template was unsuitable in the pinned runtime. Readiness checks and investigations remain deterministic Kubernetes reads in the application.

Upstream references:
- https://ai.google.dev/gemma/docs/functiongemma/formatting-and-best-practices
- https://huggingface.co/ggml-org/functiongemma-270m-it-GGUF
- https://github.com/ggml-org/llama.cpp/releases/tag/b11223

See `NOTICE.txt` and `licenses/` for redistribution terms and attribution.

## Could a model live in Git?

Yes: a sufficiently small artifact can be committed normally, and the current 292 MB model could use Git LFS. Git LFS stores the weight object separately from its Git pointer. We investigated a 35 MB alternative with two tool schemas; it confused scopes and omitted requested filters. It is not shipped. See the [reproducible small-model investigation](evaluations/needle3-2026-09-28/README.md). Bundling is technically straightforward; high command accuracy still needs domain training and held-out evaluation.
