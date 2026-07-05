//! Kubernetes Jobs backend — production (spec §3.1).
//!
//! Drives the Jobs API directly: create Job, watch pod status, stream logs.
//! TODO: implement with `kube` + `k8s-openapi`.

pub struct K8sBackend;
