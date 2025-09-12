Of course. Here is a comprehensive, multi-milestone plan that synthesizes all the strategic discussions, incorporates the simplified architecture, and assigns roles to the full, expanded team. This plan represents the "Orka Way" of building.

***

## The Orka Project: Official Roadmap

### Core Engineering Principles (The Orka Way)

This plan is guided by a set of non-negotiable principles established by the core team. Every feature and architectural decision will be measured against them.

1.  **Simplicity & Predictability First:** We will always choose the simplest, most predictable architecture. We will not add complexity (like sharding or heavy dependencies) without hard data proving a clear, unavoidable bottleneck. The core data pipeline will remain linear and easy to reason about.
2.  **Trust Is a Prerequisite, Not a Feature:** The security and stability of the tool are paramount. We will never knowingly ship a feature with a compromised or unvetted foundation. Trust is earned through robust engineering and transparency.
3.  **Optimize for the User's Inner Loop:** Every feature must be evaluated on its ability to reduce cognitive load and accelerate the core workflows of developers and operators. The goal is to make the user faster and more effective.
4.  **Leverage Our Core Architectural Advantage:** Our primary asset is the live, in-memory `WorldSnapshot`. New features should be things that are only possible, or are an order of magnitude better, because of this architecture.

---

### Milestone Alpha: The "Alpha Strike"

*   **Status:** In Progress
*   **Goal:** Ship a secure, high-impact alpha to a select group of trusted early adopters. The objective is to validate our core "wow" feature, gather critical feedback, and generate initial excitement without compromising on foundational trust.


#### **Detailed Tasks & Checklist:**

This milestone is divided into two short, focused sprints.

**Sprint 0: Minimum Viable Trust (MVT) — (1-2 Weeks)**
*Goal: Create a fundamentally secure and reliable build artifact.*

*   [x] **Harden `kubeconfig` Ingress:**
    *   Enforced strict context selection in `kubehub` (`set_context`): trims, validates allowed charset, length‑bounds, and verifies the context exists in the user's kubeconfig before switching.
    *   Clears discovery cache on context change; UI lists known contexts and switches safely.
*   [x] **Harden Data Ingress:**
    *   Added YAML input limits in `apply`:
        *   `ORKA_MAX_YAML_BYTES` (default 1 MiB) caps manifest size.
        *   `ORKA_MAX_YAML_NODES` (default 100k) caps structural complexity after parse to prevent parser DoS.
    *   Bounded Details payloads in GUI: `ORKA_DETAILS_MAX_BYTES` (default 1.5 MiB) prevents rendering oversized objects.
    *   Bounded metadata ingestion defaults in store: caps labels (128) and annotations (64) per object; respects env overrides.
    *   Secret handling hardening:
        *   Skip persisting last‑applied snapshots for `Secret` kinds by default (`ORKA_DISABLE_LASTAPPLIED` to force off globally).
        *   Redact Secret `.data` values in Details YAML; add a dedicated Secret panel with per‑key Reveal/Hide and Copy (decoded if UTF‑8, else base64) actions.

**Sprint 1: The "Wow" Feature & Community Bootstrap — (3-4 Weeks, Parallel Tracks)**
*Goal: Build our core differentiator and the channels to receive feedback on it.*

> What's Next (Sprint 1 focus)
>
> - Resource Graph (v1): list-based relationships for the selected resource (ownerReferences, mounted ConfigMaps/Secrets, ServiceAccount, related Services). Ship behind a feature flag if needed.
> - Contextual Events (v1): real-time, filtered k8s events stream in the Details pane scoped to the selected resource and its children.
> - Performance guardrails: lightweight counters/timers around new codepaths to ensure no regressions to the inner loop.
> - Docs touch-ups: add a short “Alpha README” + “Getting Started” specifically for alpha testers to try Graph/Events and provide feedback.

*   **Feature Development**
    *   [ ] **Implement Resource Graph (v1):**
        *   [ ] When a resource is selected, display a simple, list-based view of its `ownerReferences` (up the chain) and its direct relationships (mounted `ConfigMaps`/`Secrets`, `ServiceAccount`, etc.).
        *   [ ] Design the initial visual language for the graph (healthy vs. degraded links, clear typography) to ensure it's instantly readable. 
        *   Acceptance criteria:
            *   Supports at minimum: Pod, ReplicaSet, Deployment, StatefulSet, DaemonSet, Job, CronJob, Service, ConfigMap, Secret, ServiceAccount.
            *   Owner chain is correct for the selected object (e.g., Pod → ReplicaSet → Deployment; Job → CronJob; DaemonSet has no owner).
            *   Direct relationships include: Pod mounts (ConfigMaps/Secrets), Pod → ServiceAccount, Service → Pod selector resolution shows at least the count of matching Pods.
            *   Network relationships:
                *   Service (ClusterIP/Headless/NodePort/LB): resolve label selector to Pods; display matched count and readiness ratio; show ports (name/port/targetPort/protocol). For Headless, show direct Pod membership.
                *   EndpointSlice: aggregate slices for a Service; show total endpoints and ready vs notReady counts; tolerate partial data and stale slices during rollouts.
                *   Ingress (v1): list host/path rules and backends (service:port); link to referenced Service; indicate unresolved/mismatched backend service or ports; show TLS secrets linkage per host where present.
                *   ExternalName Service: display external DNS name; no Pod membership expected.
                *   Optional (stretch): highlight NetworkPolicies that select the Pod (ingress/egress), if available in snapshot.
            *   UI responds within 80ms p95 from selection to graph render on clusters up to ~2k Pods (measured as internal histogram: `graph_build_ms`).
            *   Memory overhead for graph data structures remains < 10 MB at the stated scale.
            *   Feature flag exists to toggle v1 on/off: `ORKA_GRAPH_V1=1` (default on for alpha builds).
            *   No network calls are issued synchronously on selection; graph builds from in-memory snapshot only.
    *   [ ] **Implement Contextual Events (v1):**
        *   [ ] In the details view, add a real-time, filtered stream of Kubernetes events related to the selected resource and its children. 
        *   Acceptance criteria:
            *   Filters events by involvedObject UID(s) covering the selected object and its direct descendants (e.g., Deployment ⇢ ReplicaSets ⇢ Pods) within the same namespace.
            *   First events appear within 200ms p95 after switching selection (measured as `events_first_ms`).
            *   Backlog buffer bounded by env: `ORKA_EVENTS_BUFFER` (default 500) and age cutoff `ORKA_EVENTS_MAX_AGE_SECS` (default 3600).
            *   UI controls: Pause/Resume, Clear, optional regex filter; severity color coding (Normal vs Warning) present.
            *   Uses existing watcher infra; no cluster-wide unfiltered list. Field selectors or server-side filtering are preferred when available.

#### **Deliverables for Milestone Alpha:**

*   A secure, signed binary for alpha testers.
*   A working, performant v1 of the Resource Graph and Contextual Events.

---

### Milestone 9: The Operator's Cockpit

*   **Status:** Planned
*   **Goal:** Evolve Orka from a fast viewer into an indispensable, day-to-day debugging tool for operators, SREs, and platform engineers. This milestone is about providing unparalleled insight into a cluster's state.

#### **Detailed Tasks & Checklist:**

*   [ ] **Full Multi-Cluster Management:**
    *   [ ] Implement fast context switching from the UI.
    *   [ ] Maintain separate, pre-warmed `WorldSnapshot` models for each configured context to make switching instantaneous.
    *   [ ] Add a "cluster health" overview to the main navigation panel.
*   [ ] **Resource Graph (v2 - Visualization):**
    *   [ ] Evolve the list-based view into a fully interactive, rendered graph visualization (`egui_graphs` or custom).
    *   [ ] Implement zoom, pan, and highlighting of dependency paths.
    *   [ ] Use the visual language designed by Rasmus to show resource status directly on the graph.
*   [ ] **Live Resource Metrics:**
    *   [ ] Integrate with the Kubernetes Metrics Server.
    *   [ ] Add optional "CPU" and "Memory" columns to the main tables for Pods and Nodes.
    *   [ ] Display sparkline graphs for resource usage over time in the details panel.
    *   [ ] Clearly show resource requests and limits vs. actual usage.
*   [ ] **Enhanced Documentation:**
    *   [ ] Create detailed, media-rich tutorials for the new features (Graph, Metrics).
    *   [ ] Begin building a public-facing documentation website.

---

### Milestone 10: The Developer's Workbench

*   **Status:** Planned
*   **Goal:** Directly accelerate the developer's "inner loop" (code, deploy, debug) and address professional workflows that bridge local development with the live cluster.

#### **Detailed Tasks & Checklist:**

*   [ ] **Integrated Local Manifest Preview:**
    *   [ ] Add a "Load from local..." feature to the Edit pane.
    *   [ ] Implement backend logic to run `helm template` or `kustomize build` and stream the resulting YAML into the editor.
    *   [ ] Allow the rendered manifest to be used in Orka's `diff` and `apply` workflow.
*   [ ] **The RBAC Explorer:**
    *   [ ] Create a dedicated UI to explore RBAC permissions.
    *   [ ] Implement "forward" view: select a `ServiceAccount`/`User`/`Group` and see all their permissions.
    *   [ ] Implement "reverse" view: select a resource and answer the question, "Who can perform this verb (`get`, `delete`, etc.) on this object?"
*   [ ] **Full Terminal Integration:**
    *   [ ] Finalize and stabilize the `egui_term` integration for the `Exec` tab.
    *   [ ] Ensure it's robust, performant, and supports PTY resizing correctly.
*   [ ] **Website & Community Launch:**
    *   [ ] Launch a polished website with documentation, tutorials, and clear installation instructions.
    *   [ ] Announce the first public beta and actively engage with the community for feedback.

This roadmap provides a clear path from our current state to a product that can confidently compete for the hearts and minds of the most demanding Kubernetes practitioners. It balances our desire for innovative features with the non-negotiable requirements of security, stability, and a delightful user experience.


### Deferred for now:

*   [ ] **Establish Release Pipeline:** Create an automated build process that produces checksums and GPG-signed binaries for macOS and Linux. 
*   [ ] **Create the Alpha README:** Write a clear, concise `README.md` explaining the project's vision, how to install the alpha, and the "rules of engagement" for feedback. 
*   **Track B: Foundation & Community**
    *   [ ] **Deeper Security Audit:** Continue a comprehensive review of all dependencies and data handling paths.
    *   [ ] **Performance Baselining:** Profile the alpha build under load to ensure the new features do not regress our core performance promises.
    *   [ ] **Bootstrap Community Channels:** Set up GitHub Discussions for feedback and create the initial "Getting Started" guide for alpha testers.
*   A clear channel for community feedback.
