#!/usr/bin/env bash
set -euo pipefail

# Nightly kind integration (M3): sets up a local cluster and installs a minimal
# set of operators to exercise Orka end-to-end. This script is intended to be
# run manually or as a scheduled job; CI should skip it by default.

echo "[kind-nightly] starting"

if ! command -v kind >/dev/null 2>&1; then
  echo "kind not found; please install kind (https://kind.sigs.k8s.io/)" >&2
  exit 1
fi

CLUSTER_NAME=${CLUSTER_NAME:-orka-nightly}

if ! kind get clusters | grep -q "^${CLUSTER_NAME}$"; then
  echo "[kind-nightly] creating cluster ${CLUSTER_NAME}"
  kind create cluster --name "${CLUSTER_NAME}"
else
  echo "[kind-nightly] reusing cluster ${CLUSTER_NAME}"
fi

echo "[kind-nightly] installing operators (cert-manager, kube-prometheus-stack)"
echo "Note: requires kubectl and helm with network access."
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.14.5/cert-manager.crds.yaml || true
kubectl create namespace cert-manager 2>/dev/null || true
helm repo add jetstack https://charts.jetstack.io 1>/dev/null
helm repo update 1>/dev/null
helm upgrade --install cert-manager jetstack/cert-manager --namespace cert-manager --version v1.14.5 --set installCRDs=false || true

helm repo add prometheus-community https://prometheus-community.github.io/helm-charts 1>/dev/null
helm repo update 1>/dev/null
helm upgrade --install kube-prometheus-stack prometheus-community/kube-prometheus-stack --namespace monitoring --create-namespace --set grafana.enabled=false --set prometheus.prometheusSpec.serviceMonitorSelectorNilUsesHelmValues=false || true

echo "[kind-nightly] waiting for components to be ready"
kubectl wait --for=condition=Available --timeout=180s deploy/cert-manager -n cert-manager || true
kubectl wait --for=condition=Available --timeout=180s deploy/cert-manager-webhook -n cert-manager || true
kubectl wait --for=condition=Available --timeout=180s deploy/cert-manager-cainjector -n cert-manager || true
kubectl get pods -A

echo "[kind-nightly] recording fixtures is user-defined; run orkactl discover/ls/watch"
echo "Example: ORKA_METRICS_ADDR=0.0.0.0:9090 orkactl ls v1/ConfigMap --ns default"

echo "[kind-nightly] done"
