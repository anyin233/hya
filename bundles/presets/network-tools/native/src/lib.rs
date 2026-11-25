//! Native implementation of the network built-in tool family.

use std::sync::{Arc, LazyLock};

use hya_tool::{Tool, ToolError};

mod webfetch;
mod websearch;

fn http_client() -> Result<&'static reqwest::Client, ToolError> {
    static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> =
        LazyLock::new(|| reqwest::Client::builder().user_agent("hya").build());
    CLIENT
        .as_ref()
        .map_err(|error| ToolError::Other(error.to_string()))
}

hya_tool::export_native_tool_bundle!(|out| {
    let tools: [Arc<dyn Tool>; 2] = [
        Arc::new(webfetch::WebFetchTool),
        Arc::new(websearch::WebSearchTool),
    ];
    out.extend(tools);
});
