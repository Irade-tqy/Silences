//! Silences Agent — 工具调度 + Agent 循环

pub mod agent;
pub mod checkpoint_stack;
pub mod context;
pub mod toolcall;

pub use agent::{AgentOutput, PreparedContext, run_agent_blocking, prepare_agent_context};
