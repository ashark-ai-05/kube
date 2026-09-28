# Small model investigation: Needle3

A smaller model can fit in ordinary Git. That does not establish command accuracy. This exploratory test did **not** meet the bar for replacing the current optional model or advertising high accuracy. No Needle runtime or weights are shipped or integrated into Kube.

Tested the publisher's 121M Needle3 model with its native macOS ARM64 runtime. The model file was **35,335,380 bytes**, runtime **824,792 bytes**. Both use the upstream Apache-2.0 license. Downloads were pinned to Hugging Face revision `27c0a9a5b3ca835e0b7dbeaccf555df03dac493d`:

- `needle3.cact`: SHA-256 `c9d915eca282ed42d1a09b143b592adb4cc6744ffe2d294adf5cfc5548170c38`
- `macos-arm64/needle`: SHA-256 `4091895e4dcc8936aa1c813912dd77484a6cdf73437c07dd1af1b64123ab9620`

Ran 22 synthetic queries with general tool definitions (`tools.json`), then the same queries with separate functions per resource/view (`tools-split.json`). Outputs are retained in `baseline.json` and `split-eval.json`, excluding freeform model reasoning. These are exploratory raw tool outputs, not a calibrated accuracy benchmark or Kube's validated action path.

Observed failures:

- General tools: “List pods in all namespaces” produced namespace `all` instead of the declared `*` value. “List services across every namespace” omitted the namespace argument.
- Separate tools: “List pods in all namespaces” selected deployments; “Switch namespace demo” selected a cluster; “Tell me a joke” selected unhealthy jobs.
- Health filters and previous-log arguments were also omitted on relevant requests. Changing schema layout did not resolve the problem.

To reproduce one query after independently obtaining and hash-checking the pinned artifacts:

```sh
NEEDLE_TELEMETRY=0 DO_NOT_TRACK=1 /path/to/needle \
  --model /path/to/needle3.cact --tools tools.json --system system.txt \
  --threads 2 --max 180 --fail-input-overflow \
  --prompt 'List pods in all namespaces'
```

A credible next model milestone requires task-specific training plus a held-out suite covering namespaces, cluster switching, omitted/default scope, negation, previous logs, unsupported requests and mutations. Measure complete action-and-argument correctness and abstention, not only valid JSON. Keep the deterministic interpreter and visible scope preview as the normal path.

The existing FunctionGemma file exceeds GitHub's 100 MiB normal-Git limit. It can be versioned with Git LFS, where Git stores a pointer and a separate LFS object stores the weights. The current distribution continues to package weights beside the binary as release artifacts; no large weights were added to repository history by this investigation.

Primary references: [Needle runtime](https://github.com/cactus-compute/needle), [Needle3 model](https://huggingface.co/cactus-compute/needle3), [GitHub large-file limit](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github), [Git LFS](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-git-large-file-storage), [FunctionGemma task-specific fine-tuning](https://ai.google.dev/gemma/docs/functiongemma/finetuning-with-functiongemma).
