#!/usr/bin/env bash
set -euo pipefail

# Minimal smoke test for Orka OPS logs against a local kind cluster.
# - Creates/uses a kind cluster
# - Deploys a busybox logger pod
# - Runs `orkactl ops logs` to stream lines
# - Exercises ops caps, logs regex/multi-container, and JSON outputs for one-shot ops

CLUSTER_NAME=${CLUSTER_NAME:-orka-ops-smoke}
NS=${NS:-ops-test}
POD=${POD:-logger}
DURATION_SECS=${DURATION_SECS:-6}

echo "[ops-smoke] cluster=${CLUSTER_NAME} ns=${NS} pod=${POD} duration=${DURATION_SECS}s"

if ! command -v kind >/dev/null 2>&1; then
  echo "kind not found; please install kind (https://kind.sigs.k8s.io/)" >&2
  exit 1
fi

if ! kind get clusters | grep -q "^${CLUSTER_NAME}$"; then
  echo "[ops-smoke] creating cluster ${CLUSTER_NAME}"
  kind create cluster --name "${CLUSTER_NAME}"
else
  echo "[ops-smoke] reusing cluster ${CLUSTER_NAME}"
fi

kubectl create namespace "${NS}" 2>/dev/null || true

# Wait for the default serviceaccount in the namespace (controller may take a moment)
echo "[ops-smoke] ensuring default serviceaccount in ${NS}"
ok=false
for i in {1..60}; do
  if kubectl -n "${NS}" get sa default >/dev/null 2>&1; then
    ok=true; break
  fi
  sleep 1
done
if [ "${ok}" != true ]; then
  echo "[ops-smoke] default serviceaccount missing; creating explicitly"
  kubectl -n "${NS}" create sa default 2>/dev/null || true
fi

if ! kubectl -n "${NS}" get pod "${POD}" >/dev/null 2>&1; then
  echo "[ops-smoke] creating logger pod"
  kubectl -n "${NS}" run "${POD}" --image=busybox --restart=Never -- /bin/sh -c 'i=0; while true; do echo "$(date -Iseconds) tick $i"; i=$((i+1)); sleep 1; done'
fi

echo "[ops-smoke] waiting for pod ready"
kubectl -n "${NS}" wait --for=condition=Ready --timeout=60s pod/"${POD}"

echo "[ops-smoke] running orkactl logs for ${DURATION_SECS}s"
if command -v timeout >/dev/null 2>&1; then
  timeout "${DURATION_SECS}"s cargo run -p orkactl -- --ns "${NS}" ops logs "${POD}" --tail 5 || true
else
  echo "(no 'timeout' found; press Ctrl-C to stop)"
  cargo run -p orkactl -- --ns "${NS}" ops logs "${POD}" --tail 5
fi

# Capabilities probe
echo "[ops-smoke] probing ops caps (JSON)"
cargo run -p orkactl -- --ns "${NS}" -o json ops caps || true
cargo run -p orkactl -- --ns "${NS}" -o json ops caps --gvk apps/v1/Deployment || true

# Logs with grep and JSON output
echo "[ops-smoke] logs with --grep 'tick' as JSON for ${DURATION_SECS}s"
if command -v timeout >/dev/null 2>&1; then
  timeout "${DURATION_SECS}"s cargo run -p orkactl -- --ns "${NS}" -o json ops logs "${POD}" --tail 5 --grep tick || true
else
  cargo run -p orkactl -- --ns "${NS}" -o json ops logs "${POD}" --tail 5 --grep tick || true
fi

# Multi-container pod for multi-logs
MC_POD=${MC_POD:-logger-multi}
if ! kubectl -n "${NS}" get pod "${MC_POD}" >/dev/null 2>&1; then
  echo "[ops-smoke] creating multi-container logger pod"
  cat <<EOF | kubectl -n "${NS}" apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: ${MC_POD}
spec:
  restartPolicy: Never
  containers:
  - name: app
    image: busybox
    command: ["/bin/sh", "-c", "i=0; while true; do echo \"$(date -Iseconds) app tick $i\"; i=$((i+1)); sleep 1; done"]
  - name: sidecar
    image: busybox
    command: ["/bin/sh", "-c", "i=0; while true; do echo \"$(date -Iseconds) side tick $i\"; i=$((i+1)); sleep 1; done"]
EOF
fi
echo "[ops-smoke] waiting for multi pod ready"
kubectl -n "${NS}" wait --for=condition=Ready --timeout=60s pod/"${MC_POD}"

echo "[ops-smoke] multi-logs (-c app -c sidecar) for ${DURATION_SECS}s"
if command -v timeout >/dev/null 2>&1; then
  timeout "${DURATION_SECS}"s cargo run -p orkactl -- --ns "${NS}" ops logs "${MC_POD}" -c app -c sidecar --tail 3 --grep tick || true
else
  cargo run -p orkactl -- --ns "${NS}" ops logs "${MC_POD}" -c app -c sidecar --tail 3 --grep tick || true
fi

echo "[ops-smoke] running orkactl exec (echo)"
cargo run -p orkactl -- --ns "${NS}" ops exec "${POD}" -- sh -c 'echo hello-from-exec'

# Simple HTTP pod for port-forward test
WEB_POD=${WEB_POD:-web}
WEB_PORT_REMOTE=${WEB_PORT_REMOTE:-5678}
WEB_PORT_LOCAL=${WEB_PORT_LOCAL:-18081}
if ! kubectl -n "${NS}" get pod "${WEB_POD}" >/dev/null 2>&1; then
  echo "[ops-smoke] creating http-echo pod"
  kubectl -n "${NS}" run "${WEB_POD}" --image=hashicorp/http-echo --port="${WEB_PORT_REMOTE}" -- -text=hello-from-http
fi
echo "[ops-smoke] waiting for http pod ready"
kubectl -n "${NS}" wait --for=condition=Ready --timeout=60s pod/"${WEB_POD}"

echo "[ops-smoke] starting port-forward ${WEB_PORT_LOCAL}:${WEB_PORT_REMOTE}"
set +e
cargo run -p orkactl -- --ns "${NS}" ops pf "${WEB_POD}" "${WEB_PORT_LOCAL}:${WEB_PORT_REMOTE}" > /tmp/orka_pf.log 2>&1 &
PF_PID=$!
set -e
sleep 2
echo "[ops-smoke] probing http endpoint"
if command -v curl >/dev/null 2>&1; then
  OUT=$(curl -fsS "http://127.0.0.1:${WEB_PORT_LOCAL}/" || true)
elif command -v wget >/dev/null 2>&1; then
  OUT=$(wget -qO- "http://127.0.0.1:${WEB_PORT_LOCAL}/" || true)
else
  echo "neither curl nor wget available; skipping http probe"
  OUT=""
fi
echo "[ops-smoke] http response: ${OUT}"
if [[ "${OUT}" != *"hello-from-http"* ]]; then
  echo "[ops-smoke] WARN: unexpected http echo response"
fi
kill "${PF_PID}" >/dev/null 2>&1 || true
wait "${PF_PID}" >/dev/null 2>&1 || true

# -------- scale + rollout-restart (Deployment) --------
DEPLOY=${DEPLOY:-web-deploy}
echo "[ops-smoke] ensuring deployment ${DEPLOY}"
kubectl -n "${NS}" create deployment "${DEPLOY}" \
  --image=nginx --port=80 --replicas=1 \
  --dry-run=client -o yaml | kubectl -n "${NS}" apply -f -
echo "[ops-smoke] waiting for rollout ${DEPLOY}=1"
kubectl -n "${NS}" rollout status deploy/"${DEPLOY}" --timeout=90s

echo "[ops-smoke] scaling ${DEPLOY} to 2 via orkactl"
cargo run -p orkactl -- --ns "${NS}" -o json ops scale apps/v1/Deployment "${DEPLOY}" 2
kubectl -n "${NS}" rollout status deploy/"${DEPLOY}" --timeout=90s

echo "[ops-smoke] rollout-restart ${DEPLOY} via orkactl"
cargo run -p orkactl -- --ns "${NS}" -o json ops rr apps/v1/Deployment "${DEPLOY}"
kubectl -n "${NS}" rollout status deploy/"${DEPLOY}" --timeout=120s

echo "[ops-smoke] scaling ${DEPLOY} back to 1 via orkactl"
cargo run -p orkactl -- --ns "${NS}" -o json ops scale apps/v1/Deployment "${DEPLOY}" 1
kubectl -n "${NS}" rollout status deploy/"${DEPLOY}" --timeout=90s

# -------- cordon/uncordon node --------
NODE_NAME=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
echo "[ops-smoke] cordoning node ${NODE_NAME}"
cargo run -p orkactl -- -o json ops cordon "${NODE_NAME}"
UNSCHED=$(kubectl get node "${NODE_NAME}" -o jsonpath='{.spec.unschedulable}')
echo "[ops-smoke] node unschedulable=${UNSCHED}"
echo "[ops-smoke] uncordoning node ${NODE_NAME}"
cargo run -p orkactl -- -o json ops cordon "${NODE_NAME}" --off
UNSCHED=$(kubectl get node "${NODE_NAME}" -o jsonpath='{.spec.unschedulable}')
echo "[ops-smoke] node unschedulable=${UNSCHED}"

# -------- delete pod (logger) --------
echo "[ops-smoke] deleting pod ${POD} via orkactl"
cargo run -p orkactl -- --ns "${NS}" -o json ops delete "${POD}" --grace 0 || true

# -------- optional: drain (opt-in; may fail due to PDBs) --------
if [[ "${TEST_DRAIN:-}" == "1" ]]; then
  echo "[ops-smoke] draining node (opt-in TEST_DRAIN=1)"
  if command -v timeout >/dev/null 2>&1; then
    timeout 20s ORKA_DRAIN_TIMEOUT_SECS=10 cargo run -p orkactl -- ops drain "${NODE_NAME}" || true
  else
    ORKA_DRAIN_TIMEOUT_SECS=10 cargo run -p orkactl -- ops drain "${NODE_NAME}" || true
  fi
fi

echo "[ops-smoke] done"
