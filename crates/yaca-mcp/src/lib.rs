pub mod bridge;
pub mod client;
pub mod manager;
pub mod protocol;
mod resource;

pub use client::{McpClient, McpError};
pub use manager::{McpManager, McpServerConfig, McpStatus};
