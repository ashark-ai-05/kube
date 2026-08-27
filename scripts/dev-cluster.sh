#!/usr/bin/env bash
# Create (idempotently) a local kind cluster with a `demo` namespace and a
# 3-replica `web` deployment, for the Task 11 integration tests to run against.
#
# Usage: ./scripts/dev-cluster.sh
set -euo pipefail

CLUSTER="${CLUSTER:-kube-tui-dev}"

# kind drives Kubernetes nodes as containers, so it needs a container runtime.
# Without one it fails deep inside its own preflight checks with a raw
# "exec: docker: executable file not found" error that doesn't say what to
# install. Check up front and fail with something actionable instead.
if command -v podman >/dev/null 2>&1; then
  export KIND_EXPERIMENTAL_PROVIDER="${KIND_EXPERIMENTAL_PROVIDER:-podman}"
elif command -v docker >/dev/null 2>&1; then
  : # kind defaults to docker; nothing to do.
else
  cat >&2 <<'EOF'
error: no container runtime found on PATH.

kind runs Kubernetes nodes as containers and needs either Docker or Podman
installed and running. Install one of:
  - Docker:  https://docs.docker.com/engine/install/
  - Podman:  https://podman.io/docs/installation

Then re-run this script.
EOF
  exit 1
fi

# Don't assume kubectl/kind live under /usr/local/bin — they may be installed
# per-user (e.g. ~/.local/bin). Rely on PATH resolution instead of hardcoding
# a location, and fail clearly if they're missing rather than letting `kind`
# or `kubectl` produce a bare "command not found".
for tool in kubectl kind; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: '$tool' not found on PATH. Install it and ensure its directory is on PATH." >&2
    exit 1
  fi
done

if ! kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
  kind create cluster --name "$CLUSTER"
fi

kubectl --context "kind-$CLUSTER" create namespace demo --dry-run=client -o yaml \
  | kubectl --context "kind-$CLUSTER" apply -f -
kubectl --context "kind-$CLUSTER" -n demo create deployment web \
  --image=nginx:alpine --replicas=3 --dry-run=client -o yaml \
  | kubectl --context "kind-$CLUSTER" apply -f -
kubectl --context "kind-$CLUSTER" -n demo rollout status deployment/web --timeout=120s

# An identity that can list and watch pods in `demo` and nothing else, for
# `a_forbidden_kind_is_marked_unavailable_not_retried_forever`.
#
# The token is requested through a long-lived ServiceAccount token Secret
# rather than `kubectl create token`, so the test can read it through the API
# with the client it already has instead of shelling out — and so it does not
# expire an hour into a debugging session. The token controller populates
# `.data.token` shortly after the Secret is applied; the test waits for it.
#
# Nothing here touches the user's kubeconfig: the test builds a Client from
# this token in-process, so running it cannot leave a stray context behind.
kubectl --context "kind-$CLUSTER" apply -f - <<'EOF'
apiVersion: v1
kind: ServiceAccount
metadata:
  name: restricted
  namespace: demo
---
apiVersion: v1
kind: Secret
metadata:
  name: restricted-token
  namespace: demo
  annotations:
    kubernetes.io/service-account.name: restricted
type: kubernetes.io/service-account-token
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: pods-only
  namespace: demo
rules:
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: restricted-pods-only
  namespace: demo
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: pods-only
subjects:
  - kind: ServiceAccount
    name: restricted
    namespace: demo
EOF

echo "Cluster '$CLUSTER' ready with 3 pods in namespace 'demo',"
echo "plus a pods-only ServiceAccount 'restricted' for the RBAC test."
