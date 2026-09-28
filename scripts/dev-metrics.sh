#!/usr/bin/env bash
# Metrics-server only for the disposable kind acceptance cluster.
set -euo pipefail
: "${KUBECONFIG:?Set the dedicated acceptance kubeconfig}"
context=$(kubectl config current-context)
if [[ "$context" != kind-kube-tui-* ]]; then
  echo 'Refusing metrics installation outside kind-kube-tui-*' >&2
  exit 1
fi
manifest=$(mktemp)
trap 'rm -f "$manifest"' EXIT
curl -fsSL https://github.com/kubernetes-sigs/metrics-server/releases/download/v0.9.0/components.yaml -o "$manifest"
echo "1cec29a5267809306a2c6ec74a3e449abbb705b4a8beed0c8a1963910f72c79b  $manifest" | shasum -a 256 --check
kubectl --context "$context" apply -f "$manifest"
# kind kubelet certificates are self-signed; this exception is fixture-only.
kubectl --context "$context" -n kube-system patch deployment metrics-server --type=json -p='[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--kubelet-insecure-tls"}]'
kubectl --context "$context" -n kube-system rollout status deployment/metrics-server --timeout=120s
kubectl --context "$context" wait --for=condition=Available apiservice/v1beta1.metrics.k8s.io --timeout=120s
