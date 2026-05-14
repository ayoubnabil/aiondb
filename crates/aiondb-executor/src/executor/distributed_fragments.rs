use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FragmentTarget {
    Local,
    Remote(String),
}

/// Hash partition assignment for a distributed fragment.
///
/// When set, the executor filters each fragment's result rows so that
/// only rows whose hash maps to `index` (out of `count` partitions) are
/// kept, so in a shared-storage (loopback) setup each
/// row appears in exactly one fragment's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragmentPartition {
    pub index: usize,
    pub count: usize,
}

#[derive(Clone, Debug)]
pub struct DistributedFragment {
    pub target: FragmentTarget,
    pub fragment_id: Option<u32>,
    pub shard_id: Option<u32>,
    pub plan: PhysicalPlan,
    pub partition: Option<FragmentPartition>,
}

impl DistributedFragment {
    pub fn local(plan: PhysicalPlan) -> Self {
        Self {
            target: FragmentTarget::Local,
            fragment_id: None,
            shard_id: None,
            plan,
            partition: None,
        }
    }

    pub fn remote(node_id: impl Into<String>, plan: PhysicalPlan) -> Self {
        Self {
            target: FragmentTarget::Remote(node_id.into()),
            fragment_id: None,
            shard_id: None,
            plan,
            partition: None,
        }
    }

    #[must_use]
    pub fn with_fragment_id(mut self, fragment_id: u32) -> Self {
        self.fragment_id = Some(fragment_id);
        self
    }

    #[must_use]
    pub fn with_shard_id(mut self, shard_id: u32) -> Self {
        self.shard_id = Some(shard_id);
        self
    }

    /// Assign a hash partition to this fragment so the executor keeps
    /// only rows where `hash(row) % count == index`.
    #[must_use]
    pub fn with_partition(mut self, index: usize, count: usize) -> Self {
        self.partition = Some(FragmentPartition { index, count });
        self
    }
}

/// Compute the partition index for a row by hashing all its values.
///
/// Uses the same `build_hash_key` facility that powers hash joins and
/// GROUP BY to ensure consistent partition assignment across all value
/// types.
pub fn hash_partition_for_row(row: &Row, partition_count: usize) -> usize {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    debug_assert!(partition_count > 0);
    let mut hasher = DefaultHasher::new();
    for value in &row.values {
        match build_hash_key(value) {
            Ok(key) => key.hash(&mut hasher),
            Err(_) => {
                // Unhashable values (e.g. vectors) get a constant
                // contribution.  Correctness is preserved because
                // the same row always hashes the same way.
                0u8.hash(&mut hasher);
            }
        }
    }
    let hash = hasher.finish();
    let shard = usize::try_from(hash).unwrap_or_else(|_| {
        let mixed = (hash ^ (hash >> 32)) & u64::from(u32::MAX);
        usize::try_from(mixed).unwrap_or(usize::MAX)
    });
    shard % partition_count
}

#[must_use]
pub fn distributed_fragment_target_for_index(
    fragment_index: usize,
    worker_count: usize,
    configured_remote_nodes: &[String],
) -> FragmentTarget {
    let worker_count = worker_count.max(1);
    let worker_index = fragment_index % worker_count;
    if worker_index == 0 {
        FragmentTarget::Local
    } else if !configured_remote_nodes.is_empty() {
        let node_index = (worker_index - 1) % configured_remote_nodes.len();
        FragmentTarget::Remote(configured_remote_nodes[node_index].clone())
    } else {
        FragmentTarget::Remote(format!("loopback:worker-{worker_index}"))
    }
}

pub fn assign_distributed_fragment_targets(
    fragments: &mut [DistributedFragment],
    worker_count: usize,
    configured_remote_nodes: &[String],
) {
    for (index, fragment) in fragments.iter_mut().enumerate() {
        fragment.target =
            distributed_fragment_target_for_index(index, worker_count, configured_remote_nodes);
    }
}

#[must_use]
pub fn format_fragment_target(target: &FragmentTarget) -> String {
    match target {
        FragmentTarget::Local => "local".to_owned(),
        FragmentTarget::Remote(node_id) => format!("remote({node_id})"),
    }
}

pub trait FragmentDispatcher: Send + Sync {
    fn execute_fragment(
        &self,
        fragment: &DistributedFragment,
        executor: &Executor,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult>;
}

pub type RemoteFragmentHandler = dyn Fn(&DistributedFragment, &Executor, &ExecutionContext) -> DbResult<ExecutionResult>
    + Send
    + Sync;

#[derive(Default)]
pub struct RegisteredRemoteFragmentDispatcher {
    remote_handlers: HashMap<String, Arc<RemoteFragmentHandler>>,
    default_remote_handler: Option<Arc<RemoteFragmentHandler>>,
    allow_loopback_remote_targets: bool,
    node_registry: Option<Arc<super::node_registry::NodeRegistry>>,
}

impl RegisteredRemoteFragmentDispatcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            remote_handlers: HashMap::new(),
            default_remote_handler: None,
            allow_loopback_remote_targets: true,
            node_registry: None,
        }
    }

    #[must_use]
    pub fn with_remote_handler(
        mut self,
        node_id: impl Into<String>,
        handler: Arc<RemoteFragmentHandler>,
    ) -> Self {
        self.register_remote_handler(node_id, handler);
        self
    }

    pub fn register_remote_handler(
        &mut self,
        node_id: impl Into<String>,
        handler: Arc<RemoteFragmentHandler>,
    ) {
        self.remote_handlers.insert(node_id.into(), handler);
    }

    #[must_use]
    pub fn with_default_remote_handler(mut self, handler: Arc<RemoteFragmentHandler>) -> Self {
        self.default_remote_handler = Some(handler);
        self
    }

    pub fn set_default_remote_handler(&mut self, handler: Arc<RemoteFragmentHandler>) {
        self.default_remote_handler = Some(handler);
    }

    #[must_use]
    pub fn with_node_registry(
        mut self,
        node_registry: Arc<super::node_registry::NodeRegistry>,
    ) -> Self {
        self.node_registry = Some(node_registry);
        self
    }

    #[must_use]
    pub fn with_loopback_remote_targets(mut self, enabled: bool) -> Self {
        self.allow_loopback_remote_targets = enabled;
        self
    }
}

impl std::fmt::Debug for RegisteredRemoteFragmentDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredRemoteFragmentDispatcher")
            .field("registered_remote_handlers", &self.remote_handlers.len())
            .field(
                "has_default_remote_handler",
                &self.default_remote_handler.is_some(),
            )
            .field(
                "allow_loopback_remote_targets",
                &self.allow_loopback_remote_targets,
            )
            .field("has_node_registry", &self.node_registry.is_some())
            .finish()
    }
}

impl FragmentDispatcher for RegisteredRemoteFragmentDispatcher {
    fn execute_fragment(
        &self,
        fragment: &DistributedFragment,
        executor: &Executor,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match &fragment.target {
            FragmentTarget::Local => {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                executor.execute(&fragment.plan, &fragment_context)
            }
            FragmentTarget::Remote(node_id)
                if self.allow_loopback_remote_targets && node_id.starts_with("loopback:") =>
            {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                executor.execute(&fragment.plan, &fragment_context)
            }
            FragmentTarget::Remote(node_id) => {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                if let Some(registry) = &self.node_registry {
                    if registry.get(node_id).is_some() {
                        return registry.execute_fragment(fragment, executor, &fragment_context);
                    }
                }
                let handler = if let Some(handler) = self.remote_handlers.get(node_id) {
                    handler
                } else if let Some(handler) = self.default_remote_handler.as_ref() {
                    handler
                } else {
                    return Err(DbError::feature_not_supported(format!(
                        "remote fragment execution target \"{node_id}\" is not registered",
                    )));
                };
                handler(fragment, executor, &fragment_context)
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct LocalFragmentDispatcher;

impl FragmentDispatcher for LocalFragmentDispatcher {
    fn execute_fragment(
        &self,
        fragment: &DistributedFragment,
        executor: &Executor,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match &fragment.target {
            FragmentTarget::Local => {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                executor.execute(&fragment.plan, &fragment_context)
            }
            FragmentTarget::Remote(node_id) if node_id.starts_with("loopback:") => {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                executor.execute(&fragment.plan, &fragment_context)
            }
            FragmentTarget::Remote(node_id) => Err(DbError::feature_not_supported(format!(
                "remote fragment execution target \"{node_id}\" is not configured",
            ))),
        }
    }
}
