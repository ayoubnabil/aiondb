#![recursion_limit = "256"]
#![allow(
    clippy::needless_continue,
    clippy::used_underscore_binding,
    clippy::assigning_clones,
    clippy::cast_lossless,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::elidable_lifetime_names,
    clippy::items_after_statements,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::no_effect_underscore_binding,
    clippy::redundant_closure_for_method_calls,
    clippy::return_self_not_must_use,
    clippy::self_only_used_in_recursion,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::uninlined_format_args,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::wildcard_imports
)]

pub mod context;
pub mod distributed_runtime;
pub mod executor;
pub mod result;
pub mod row_stream;
pub(crate) mod spill;

pub use context::{ExecutionContext, ParallelScanPartition, SequenceSessionState, SessionSettings};
pub use distributed_runtime::DistributedQueryRuntime;
pub use executor::node_registry;
pub use executor::{
    assign_distributed_fragment_targets, distributed_fragment_target_for_index,
    format_copy_text_value, format_fragment_target, hash_partition_for_row, parse_copy_text_value,
    DistributedFragment, Executor, FragmentDispatcher, FragmentPartition, FragmentTarget,
    RegisteredRemoteFragmentDispatcher, RemoteFragmentHandler,
};
pub use result::{ExecutionResult, ResultChunk};
