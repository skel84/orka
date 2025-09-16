#![forbid(unsafe_code)]

use eframe::egui;
use orka_api::ResourceKind;

use super::OrkaGuiApp;
use crate::util::gvk_label;

impl OrkaGuiApp {
    pub(crate) fn ui_kind_tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("kind_tree_scroll")
            .show(ui, |ui| {
                // Curated built-in categories (collapsed by default)
                self.ui_curated_category(
                    ui,
                    "Workloads",
                    &[
                        ("", "Pod", "Pods", true),
                        ("apps", "Deployment", "Deployments", true),
                        ("apps", "DaemonSet", "Daemon Sets", true),
                        ("apps", "StatefulSet", "Stateful Sets", true),
                        ("apps", "ReplicaSet", "Replica Sets", true),
                        ("", "ReplicationController", "Replication Controllers", true),
                        ("batch", "Job", "Jobs", true),
                        ("batch", "CronJob", "Cron Jobs", true),
                    ],
                );
                self.ui_curated_category(
                    ui,
                    "Config",
                    &[
                        ("", "ConfigMap", "Config Maps", true),
                        ("", "Secret", "Secrets", true),
                        ("", "ResourceQuota", "Resource Quotas", true),
                        ("", "LimitRange", "Limit Ranges", true),
                        (
                            "autoscaling",
                            "HorizontalPodAutoscaler",
                            "Horizontal Pod Autoscalers",
                            true,
                        ),
                        (
                            "autoscaling.k8s.io",
                            "VerticalPodAutoscaler",
                            "Vertical Pod Autoscalers",
                            true,
                        ),
                        (
                            "policy",
                            "PodDisruptionBudget",
                            "Pod Disruption Budgets",
                            true,
                        ),
                        (
                            "scheduling.k8s.io",
                            "PriorityClass",
                            "Priority Classes",
                            false,
                        ),
                        ("node.k8s.io", "RuntimeClass", "Runtime Classes", false),
                        ("coordination.k8s.io", "Lease", "Leases", true),
                        (
                            "admissionregistration.k8s.io",
                            "MutatingWebhookConfiguration",
                            "Mutating Webhook Configurations",
                            false,
                        ),
                        (
                            "admissionregistration.k8s.io",
                            "ValidatingWebhookConfiguration",
                            "Validating Webhook Configurations",
                            false,
                        ),
                    ],
                );
                self.ui_curated_category(
                    ui,
                    "Network",
                    &[
                        ("", "Service", "Services", true),
                        ("", "Endpoints", "Endpoints", true),
                        ("networking.k8s.io", "Ingress", "Ingresses", true),
                        (
                            "networking.k8s.io",
                            "IngressClass",
                            "Ingress Classes",
                            false,
                        ),
                        (
                            "networking.k8s.io",
                            "NetworkPolicy",
                            "Network Policies",
                            true,
                        ),
                    ],
                );
                self.ui_curated_category(
                    ui,
                    "Storage",
                    &[
                        (
                            "",
                            "PersistentVolumeClaim",
                            "Persistent Volume Claims",
                            true,
                        ),
                        ("", "PersistentVolume", "Persistent Volumes", false),
                        ("storage.k8s.io", "StorageClass", "Storage Classes", false),
                    ],
                );
                self.ui_curated_category(
                    ui,
                    "Access Control",
                    &[
                        ("", "ServiceAccount", "Service Accounts", true),
                        (
                            "rbac.authorization.k8s.io",
                            "ClusterRole",
                            "Cluster Roles",
                            false,
                        ),
                        ("rbac.authorization.k8s.io", "Role", "Roles", true),
                        (
                            "rbac.authorization.k8s.io",
                            "ClusterRoleBinding",
                            "Cluster Role Bindings",
                            false,
                        ),
                        (
                            "rbac.authorization.k8s.io",
                            "RoleBinding",
                            "Role Bindings",
                            true,
                        ),
                    ],
                );
                // Singletons
                if let Some(idx) = self.find_kind_index("", "Namespace") {
                    self.ui_single_item(ui, idx, "Namespaces");
                }
                if let Some(idx) = self.find_kind_index("", "Node") {
                    self.ui_single_item(ui, idx, "Nodes");
                }
                if let Some(idx) = self
                    .find_kind_index("events.k8s.io", "Event")
                    .or_else(|| self.find_kind_index("", "Event"))
                {
                    self.ui_single_item(ui, idx, "Events");
                }

                // Custom Resources (grouped by API group), collapsed by default
                self.ui_crd_section(ui);
            });
    }

    fn ui_single_item(&mut self, ui: &mut egui::Ui, idx: usize, label: &str) {
        let selected = self.selection.selected_idx == Some(idx);
        let resp = ui.selectable_label(selected, label);
        if resp.clicked() {
            self.on_select_idx(idx);
        }
    }

    fn ui_curated_category(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        entries: &[(&str, &str, &str, bool)],
    ) {
        egui::CollapsingHeader::new(title)
            .default_open(false)
            .show(ui, |ui| {
                for (group, kind, label, namespaced) in entries {
                    let rk = ResourceKind {
                        group: (*group).to_string(),
                        version: "v1".to_string(),
                        kind: (*kind).to_string(),
                        namespaced: *namespaced,
                    };
                    let is_sel = self
                        .current_selected_kind()
                        .map(|k| gvk_label(k) == gvk_label(&rk))
                        .unwrap_or(false);
                    let resp = ui.selectable_label(is_sel, *label);
                    if resp.clicked() {
                        self.on_select_gvk(rk.clone());
                    }
                }
            });
    }

    fn is_builtin_group(group: &str) -> bool {
        if group.is_empty() {
            return true;
        }
        matches!(
            group,
            "apps"
                | "batch"
                | "autoscaling"
                | "autoscaling.k8s.io"
                | "policy"
                | "rbac.authorization.k8s.io"
                | "networking.k8s.io"
                | "storage.k8s.io"
                | "node.k8s.io"
                | "coordination.k8s.io"
                | "admissionregistration.k8s.io"
                | "events.k8s.io"
                | "scheduling.k8s.io"
                | "apiregistration.k8s.io"
                | "authentication.k8s.io"
                | "authorization.k8s.io"
                | "discovery.k8s.io"
                | "flowcontrol.apiserver.k8s.io"
        )
    }

    fn ui_crd_section(&mut self, ui: &mut egui::Ui) {
        use std::collections::BTreeMap;
        // group -> Vec<(idx, kind)>
        let mut groups: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
        for (idx, k) in self.discovery.kinds.iter().enumerate() {
            if Self::is_builtin_group(&k.group) {
                continue;
            }
            let entry = groups.entry(k.group.clone()).or_default();
            entry.push((idx, k.kind.clone()));
        }
        if groups.is_empty() {
            return;
        }
        egui::CollapsingHeader::new("Custom Resources")
            .default_open(false)
            .show(ui, |ui| {
                for (group, mut kinds) in groups.into_iter() {
                    kinds.sort_by(|a, b| a.1.cmp(&b.1));
                    egui::CollapsingHeader::new(group)
                        .default_open(false)
                        .show(ui, |ui| {
                            for (idx, name) in kinds.into_iter() {
                                let selected = self.selection.selected_idx == Some(idx);
                                let resp = ui.selectable_label(selected, name);
                                if resp.clicked() {
                                    self.on_select_idx(idx);
                                }
                            }
                        });
                }
            });
    }

    fn find_kind_index(&self, group: &str, kind: &str) -> Option<usize> {
        // Prefer v1 when multiple versions exist
        let mut candidate: Option<usize> = None;
        for (idx, k) in self.discovery.kinds.iter().enumerate() {
            if k.kind == kind && ((group.is_empty() && k.group.is_empty()) || k.group == group) {
                if k.version == "v1" {
                    return Some(idx);
                }
                candidate = Some(idx);
            }
        }
        candidate
    }

    fn on_select_idx(&mut self, idx: usize) {
        if let Some(k) = self.discovery.kinds.get(idx).cloned() {
            tracing::info!(gvk = %gvk_label(&k), "ui: kind clicked");
            self.selection.selected_kind = Some(k);
            self.selection.selected_idx = Some(idx);
        }
    }

    fn on_select_gvk(&mut self, rk: ResourceKind) {
        tracing::info!(gvk = %gvk_label(&rk), "ui: kind clicked");
        self.selection.selected_kind = Some(rk);
        self.selection.selected_idx = None;
    }

    pub(crate) fn current_selected_kind(&self) -> Option<&ResourceKind> {
        match self.selection.selected_kind.as_ref() {
            Some(k) => Some(k),
            None => self
                .selection
                .selected_idx
                .and_then(|i| self.discovery.kinds.get(i)),
        }
    }
}
