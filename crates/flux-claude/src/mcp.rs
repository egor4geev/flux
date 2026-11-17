//! Flux's own MCP server for Claude (part 9.2): an in-process ("sdk") server named `flux`. The
//! CLI learns its name from `initialize.sdkMcpServers` and talks to it in JSON-RPC wrapped in
//! `mcp_message` control requests (PROTOCOL.md §7.3); every message gets one reply, the
//! `mcp_response` of the control response. Tool calls go to the window ([`McpReply::Call`]) —
//! the language servers, the tabs, the plugins' tools (API 0.5) — and come back as a
//! [`ToolOutput`].

use serde_json::{Value, json};

/// The server's name: Claude sees its tools as `mcp__flux__<tool>`.
pub const SERVER: &str = "flux";

/// The protocol version Flux answers with when the CLI doesn't name one.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// The longest name a tool may have with the server's prefix (`mcp__flux__…`): the API's limit.
pub const MAX_TOOL_NAME: usize = 64;

/// A tool Claude may call: Flux's own, or a plugin's (its name has the plugin's prefix).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// The name without the server's prefix: "get_diagnostics", "fluxgram_read_chat".
    pub name: String,
    /// For people: "Get Diagnostics".
    pub title: String,
    /// For the model: what the tool does and when to use it.
    pub description: String,
    /// The JSON schema of the arguments.
    pub input_schema: Value,
    /// It only reads: Claude calls it without asking the user.
    pub read_only: bool,
}

/// What a tool call gives Claude.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        ToolOutput {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        ToolOutput {
            text: text.into(),
            is_error: true,
        }
    }
}

/// What to do with a message of the CLI to the server.
#[derive(Debug, Clone, PartialEq)]
pub enum McpReply {
    /// Answer at once with this `mcp_response`.
    Respond(Value),
    /// A tool call: the window runs it and answers with [`McpServer::result`].
    Call {
        id: Value,
        name: String,
        arguments: Value,
        /// The `tool_use` block of the call in the conversation (`_meta.claudecode/toolUseId`):
        /// ties the call to its step in the chat.
        tool_use_id: Option<String>,
    },
}

/// The JSON-RPC side of the server: `initialize`, `tools/list`, `tools/call`, notifications.
#[derive(Debug, Clone, Default)]
pub struct McpServer {
    tools: Vec<ToolSpec>,
}

impl McpServer {
    pub fn new(tools: Vec<ToolSpec>) -> Self {
        McpServer { tools }
    }

    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    /// The tool behind a name, if the server has it.
    pub fn tool(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn handle(&self, message: &Value) -> McpReply {
        let method = message["method"].as_str().unwrap_or_default();
        // A notification has no id and wants no answer; the control request still does.
        let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
            return McpReply::Respond(json!({ "jsonrpc": "2.0", "result": {}, "id": 0 }));
        };
        match method {
            "initialize" => {
                let version = message["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or(PROTOCOL_VERSION);
                McpReply::Respond(success(
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
                    }),
                ))
            }
            "ping" => McpReply::Respond(success(&id, json!({}))),
            "tools/list" => {
                let tools: Vec<Value> = self
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "title": tool.title,
                            "description": tool.description,
                            "inputSchema": tool.input_schema,
                            "annotations": { "title": tool.title, "readOnlyHint": tool.read_only },
                        })
                    })
                    .collect();
                McpReply::Respond(success(&id, json!({ "tools": tools })))
            }
            "tools/call" => {
                let params = &message["params"];
                let name = params["name"].as_str().unwrap_or_default();
                if self.tool(name).is_none() {
                    return McpReply::Respond(McpServer::result(
                        &id,
                        &ToolOutput::error(format!("Flux has no tool {name}.")),
                    ));
                }
                let arguments = match &params["arguments"] {
                    Value::Null => json!({}),
                    arguments => arguments.clone(),
                };
                let tool_use_id = params["_meta"]["claudecode/toolUseId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(str::to_string);
                McpReply::Call {
                    id,
                    name: name.to_string(),
                    arguments,
                    tool_use_id,
                }
            }
            method => McpReply::Respond(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {method}") },
            })),
        }
    }

    /// The `mcp_response` of a finished tool call.
    pub fn result(id: &Value, output: &ToolOutput) -> Value {
        success(
            id,
            json!({
                "content": [{ "type": "text", "text": output.text }],
                "isError": output.is_error,
            }),
        )
    }
}

fn success(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The name Claude calls a Flux tool by: `mcp__flux__get_diagnostics`.
pub fn tool_name(tool: &str) -> String {
    format!("mcp__{SERVER}__{tool}")
}

/// The Flux tool behind a name Claude used, if it is one: `mcp__flux__x` → `x`.
pub fn flux_tool(name: &str) -> Option<&str> {
    name.strip_prefix("mcp__")?
        .strip_prefix(SERVER)?
        .strip_prefix("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> McpServer {
        McpServer::new(vec![
            ToolSpec {
                name: "open_file".into(),
                title: "Open File".into(),
                description: "Opens a file in Flux.".into(),
                input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
                read_only: true,
            },
            // A plugin's tool: its name has the plugin's prefix; it changes something.
            ToolSpec {
                name: "fluxgram_send_reply".into(),
                title: "Reply in a Chat".into(),
                description: "Sends a reply once the user confirms it.".into(),
                input_schema: json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
                read_only: false,
            },
        ])
    }

    #[test]
    fn answers_the_handshake_and_the_tool_list() {
        let server = server();
        let initialize = json!({
            "method": "initialize", "jsonrpc": "2.0", "id": 0,
            "params": { "protocolVersion": "2025-11-25", "capabilities": {} },
        });
        let McpReply::Respond(reply) = server.handle(&initialize) else {
            panic!("initialize is answered at once");
        };
        assert_eq!(reply["id"], 0);
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(reply["result"]["serverInfo"]["name"], "flux");

        let notification = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(
            server.handle(&notification),
            McpReply::Respond(json!({ "jsonrpc": "2.0", "result": {}, "id": 0 }))
        );

        let list = json!({ "method": "tools/list", "jsonrpc": "2.0", "id": 1 });
        let McpReply::Respond(reply) = server.handle(&list) else {
            panic!("tools/list is answered at once");
        };
        assert_eq!(reply["result"]["tools"][0]["name"], "open_file");
        assert_eq!(
            reply["result"]["tools"][0]["annotations"]["readOnlyHint"],
            true
        );
        let plugin = &reply["result"]["tools"][1];
        assert_eq!(plugin["name"], "fluxgram_send_reply");
        assert_eq!(plugin["title"], "Reply in a Chat");
        assert_eq!(
            plugin["inputSchema"]["properties"]["text"]["type"],
            "string"
        );
        assert_eq!(plugin["annotations"]["readOnlyHint"], false);
    }

    #[test]
    fn a_call_goes_to_the_window_and_comes_back_as_content() {
        let server = server();
        let call = json!({
            "method": "tools/call", "jsonrpc": "2.0", "id": 2,
            "params": { "name": "open_file", "arguments": { "path": "a.rs" },
                        "_meta": { "claudecode/toolUseId": "toolu_1", "progressToken": 2 } },
        });
        assert_eq!(
            server.handle(&call),
            McpReply::Call {
                id: json!(2),
                name: "open_file".into(),
                arguments: json!({ "path": "a.rs" }),
                tool_use_id: Some("toolu_1".into()),
            }
        );
        let result = McpServer::result(&json!(2), &ToolOutput::text("Opened a.rs."));
        assert_eq!(result["id"], 2);
        assert_eq!(result["result"]["content"][0]["text"], "Opened a.rs.");
        assert_eq!(result["result"]["isError"], false);

        let unknown = json!({ "method": "tools/call", "jsonrpc": "2.0", "id": 3,
                              "params": { "name": "nope" } });
        let McpReply::Respond(reply) = server.handle(&unknown) else {
            panic!("an unknown tool is answered at once");
        };
        assert_eq!(reply["result"]["isError"], true);
    }

    #[test]
    fn a_plugin_tool_call_keeps_its_tool_use_id() {
        let server = server();
        let call = json!({
            "method": "tools/call", "jsonrpc": "2.0", "id": 4,
            "params": { "name": "fluxgram_send_reply", "arguments": { "text": "Hi" },
                        "_meta": { "claudecode/toolUseId": "toolu_9" } },
        });
        let McpReply::Call {
            name, tool_use_id, ..
        } = server.handle(&call)
        else {
            panic!("a plugin's tool goes to the window");
        };
        assert_eq!(name, "fluxgram_send_reply");
        assert_eq!(tool_use_id.as_deref(), Some("toolu_9"));

        // Without `_meta` (an older CLI) the call still goes, untied to a step.
        let bare = json!({ "method": "tools/call", "jsonrpc": "2.0", "id": 5,
                           "params": { "name": "fluxgram_send_reply" } });
        let McpReply::Call {
            arguments,
            tool_use_id,
            ..
        } = server.handle(&bare)
        else {
            panic!("a plugin's tool goes to the window");
        };
        assert_eq!(arguments, json!({}));
        assert_eq!(tool_use_id, None);
        assert!(tool_name("fluxgram_send_reply").len() <= MAX_TOOL_NAME);
    }

    #[test]
    fn names_of_flux_tools() {
        assert_eq!(tool_name("get_diagnostics"), "mcp__flux__get_diagnostics");
        assert_eq!(
            flux_tool("mcp__flux__get_diagnostics"),
            Some("get_diagnostics")
        );
        assert_eq!(flux_tool("mcp__other__x"), None);
        assert_eq!(flux_tool("Read"), None);
    }
}
