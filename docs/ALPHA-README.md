Orka Alpha — Getting Started (Graph v1)

What’s new
- Resource Graph (v1): A new “Graph” tab in the Details pane that shows a list-based view of relationships for the selected resource.

How to try it
- Launch the GUI and select a kind (e.g., Pods or Services).
- Click a row in Results to open Details.
- Switch to the “Graph” tab to see:
  - Owner Chain: controller owners up the chain (e.g., Pod → ReplicaSet → Deployment).
  - Direct: for Pods, mounted ConfigMaps/Secrets and the ServiceAccount; for Services, the count of Pods matching the selector.

Notes
- This is a v1 list-based graph focused on clarity and speed.
- The graph fetches owners on-demand and bounds traversal to 5 steps.
- Event streaming scoped to the selection is planned in the next iteration of Sprint 1.

Troubleshooting
- If no data appears, click “Refresh” in the Graph tab.
- Ensure your kube context points to a cluster with workloads and that you have list/get permissions.
