pub mod auth;
pub mod config;
pub mod discovery;
pub mod namespaces;
pub mod registry;
pub use auth::{
    AuthMethod, ConnectOptions, connect_with, disable_interactive_exec, kubeconfig_paths_from_env,
    merge_kubeconfigs,
};
pub use config::{ContextInfo, connect, contexts_from_yaml, load_contexts};
pub use namespaces::{NamespaceListError, is_valid_namespace_name, list_namespaces};
pub use registry::{ClusterEntry, ClusterId, ClusterRegistry, ConnectionState};
