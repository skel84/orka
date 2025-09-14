Of course. Here is the updated, comprehensive roadmap that incorporates the "Atlas Initiative" as the central "wow" feature, details the GPU acceleration strategy, and integrates the progress already made. This plan reflects the consensus of the entire team.

***

## The Orka Project: Official Roadmap (Revised)

### Core Engineering Principles (The Orka Way)

This plan is guided by a set of non-negotiable principles established by the core team. Every feature and architectural decision will be measured against them.

1.  **Simplicity & Predictability First:** We will always choose the simplest, most predictable architecture. We will not add complexity (like sharding or heavy dependencies) without hard data proving a clear, unavoidable bottleneck.
2.  **Trust Is a Prerequisite, Not a Feature:** The security and stability of the tool are paramount. We will never knowingly ship a feature with a compromised or unvetted foundation. Trust is earned through robust engineering and transparency.
3.  **Optimize for the User's Inner Loop:** Every feature must be evaluated on its ability to reduce cognitive load and accelerate the core workflows of developers and operators. The goal is to make the user faster and more effective.
4.  **Leverage Our Core Architectural Advantage:** Our primary asset is the live, in-memory `WorldSnapshot`. New features should be things that are only possible, or are an order of magnitude better, because of this architecture.

---

### Milestone Alpha: The "Alpha Strike"

*   **Status:** **Completed**
*   **Goal:** Ship a secure, high-impact alpha to a select group of trusted early adopters. The objective was to validate our core "wow" feature, gather critical feedback, and generate initial excitement without compromising on foundational trust.

#### **Sprint 0: Minimum Viable Trust (MVT) — Completed**
*Goal: Create a fundamentally secure and reliable build artifact.*

*   [x] **Harden `kubeconfig` Ingress:** Enforced strict context selection, validation, and cache clearing on context change.
*   [x] **Harden Data Ingress:** Added YAML input limits (`ORKA_MAX_YAML_BYTES`, `ORKA_MAX_YAML_NODES`), bounded Details payloads, and capped metadata ingestion to prevent parser-based DoS attacks.
*   [x] **Harden Secret Handling:** Disabled `last-applied` persistence for `Secret` kinds by default and implemented redaction/reveal functionality in the UI to prevent accidental exposure.

#### **Sprint 1: The "Wow" Feature Foundation — Completed**
*Goal: Build the data model and initial UI for our core differentiator.*

*   [x] **Implement Resource Graph (v1 - List-Based):**
    *   A new "Graph" tab was added to the Details pane, displaying a simple, list-based view of a resource's `ownerReferences` (up the chain) and its direct relationships (mounted `ConfigMaps`/`Secrets`, `ServiceAccount`, etc.).
    *   This initial version validated that the relationships can be computed efficiently from the in-memory `WorldSnapshot` with no synchronous network calls.
    *   Performance guardrail metric `ui_graph_build_ms` was added.
    *   An `ALPHA-README.md` was created to guide testers.

---

### Milestone 11: The Atlas Initiative

*   **Status:** In Progress
*   **Goal:** Evolve the list-based graph into a fully interactive, visually stunning, and GPU-accelerated "Atlas View," establishing it as the primary, but not exclusive, way to interact with Orka. This is our core "wow" feature.

#### **Phase 1: Build the Interactive Atlas**

*   [x] **Classic/Atlas Toggle (Details > Graph):** Users can switch between the existing list view and the new "Atlas" interactive view in the Details pane.
*   [x] **Background Graph Model (owner/related):** Build an in-memory graph model for the selected resource (owner chain + direct relationships such as ServiceAccount, ConfigMaps/Secrets, and Pods via Service selectors).
*   [x] **Minimal Interactive Renderer:** Implement an internal egui-based canvas with pan/zoom, colored nodes, edges, and clickable node feedback.
*   [x] **Implement Progressive Disclosure (MVP):**
    *   Global: Namespaces grid → expand shows 5 kind badges (Pods/Deployments/Services/ConfigMaps/Secrets) with live counts. No per-item lists in MVP to avoid hairball; fast and stable. Kill‑switch via `ORKA_ATLAS=0`.
    *   Details: Root + owner chain; related grouped by kind with expand‑on‑click and top‑N items. Items (ConfigMap/Secret/ServiceAccount) are clickable and open Details. One‑shot auto‑fit on open; explicit "Fit" button.
*   [ ] **Integrate Command Palette:** The Command Palette will become the primary navigation tool for the Atlas. As the user types, matching nodes will be highlighted in real-time. Hitting Enter will pan and zoom the view to focus on the selected resource and its immediate neighbors.
*   [ ] **Develop the Visual Language:** Design a clear and intuitive visual system of colors, icons, and line styles to represent resource health (e.g., green for `Running`, yellow for `Pending`, red for `Failed`), relationships, and status.
*   [x] **Library Evaluation (Optional):** Evaluated `egui_graphs` and removed it. We standardised on the internal painter for predictable layout, zero extra deps, and simpler UX. A GPU path remains optional for later.

#### **Phase 2: Progressive Acceleration (The God-Tier Experience)**
*Goal: Deliver a fluid, 120Hz experience on modern hardware by leveraging GPU acceleration, with a robust CPU fallback.*

*   [ ] **Maximize CPU Performance:**
    *   [ ] Move the graph layout algorithm to a dedicated thread pool (e.g., using `rayon`) to keep the UI thread free.
    *   [ ] Optimize hot paths in the layout calculation using SIMD to achieve maximum performance on the CPU. This ensures a great baseline experience on all hardware.
*   [ ] **Implement Optional GPU Compute Offload:**
    *   [ ] Behind a feature flag, implement a `wgpu`-based compute shader to perform the N-body simulation for the graph layout on the GPU.
    *   [ ] This will be automatically enabled on supported hardware (Apple Silicon, Linux laptops with Vulkan, Windows with DX12), providing a 10-100x speedup for this specific task.
*   [ ] **Implement GPU Rendering Pipeline:**
    *   [ ] Offload the drawing of all nodes and edges to the GPU using custom `wgpu` primitives. This will dramatically reduce CPU usage during rendering and enable perfectly smooth animations.
*   [ ] **Ensure Robust Fallbacks & Testing:**
    *   [ ] The application *must* detect GPU/driver failures at startup and seamlessly fall back to the optimized CPU-only mode. The user should never see a crash, only a notification.
    *   [ ] Establish a CI test matrix that includes both X11 and Wayland on Linux, and targets Apple Silicon to catch platform-specific regressions.

—

Delivered in this milestone so far (MVP):

* Atlas baseline (Details > Graph): toggle, background model builder, pan/zoom renderer.
* Progressive Disclosure:
  * Global: Namespaces → Kind badges with counts (no item lists). Fast snapshot fallback; resilient to missing watchers.
  * Details: Grouped related, expand‑on‑click with top‑N items; clickable items open Details; one‑shot auto‑fit + "Fit" control. Ring layout around the root to keep groups near the main chain.
* Safety/ops: `ORKA_ATLAS` env to disable Atlas completely if needed.

Cut from MVP (post‑MVP targets):

* Palette highlighting and jump-to in Atlas.
* Services → Pods item expansion in Details; large lists paging.
* Visual language pass (status colors, edge routing, icons).
* Optional GPU/compute and physics layout.

---

### Milestone 2: The Operator's Cockpit

*   **Status:** Planned
*   **Goal:** Build on the Atlas by providing unparalleled, context-aware insight for debugging and operational tasks.

#### **Detailed Tasks & Checklist:**

*   [ ] **Full Multi-Cluster Management:** Implement fast context switching from the UI, maintaining separate, pre-warmed `WorldSnapshot` models for each context to make switching instantaneous.
*   [ ] **Live Status & Metrics on Atlas:** Overlay real-time status and resource metrics (CPU/Memory from the Metrics Server) directly onto the nodes in the Atlas view.
*   [ ] **Contextual Events (v1):** In the details panel, add a real-time, filtered stream of Kubernetes events related to the selected resource and its children, using bounded buffers to remain performant.
*   [ ] **The RBAC Explorer:** Create a dedicated UI to explore RBAC permissions, allowing users to select a principal (User, ServiceAccount) and see what they can do, or select a resource and see who can act upon it.

---

### Milestone 3: The Developer's Workbench

*   **Status:** Planned
*   **Goal:** Directly accelerate the developer's "inner loop" and bridge local development with the live cluster.

#### **Detailed Tasks & Checklist:**

*   [ ] **Integrated Local Manifest Preview:** Add a "Load from local..." feature that can run `helm template` or `kustomize build` and pipe the output directly into Orka's diff and apply workflow.
*   [ ] **Full Terminal Integration:** Finalize and stabilize the `egui_term` integration for the `Exec` tab, ensuring it is robust, performant, and supports PTY resizing correctly on all platforms.
*   [ ] **Website & Community Launch:** Announce the first public beta, launching a polished website with documentation, tutorials, and clear installation instructions to actively engage with the community.

---

### Deferred (Post-Beta)

*   [ ] **Complete Release Automation:** Finalize the GPG signing and automated publishing of release artifacts to platforms like Homebrew and Scoop.
*   [ ] **Formal Community Channels:** Establish and moderate official channels like Discord or Slack for community support and engagement.
*   [ ] **Comprehensive Security Audit:** Engage a third party for a full security audit prior to a 1.0 release.
