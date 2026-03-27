//! Child session spawning and completion propagation helpers.

mod completion;
mod create;

pub use completion::enqueue_child_completion;
pub(crate) use completion::{latest_assistant_response, should_enqueue_child_completion};
pub(crate) use create::{ChildSessionMetadata, parse_child_session_metadata};
pub use create::{SpawnDrainResult, SpawnRequest, SpawnResult, spawn_child};
