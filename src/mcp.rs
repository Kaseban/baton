//! baton MCP server.
//!
//! Exposes four tools to any MCP-compatible coding agent:
//!   - `list_sessions` — scan all agents, return a unified list
//!   - `convert_session` — convert one agent's session into another's format
//!   - `import_to_target` — convert + invoke the target agent's import command
//!   - `detect_format` — sniff a session file/dir and report the agent

#[cfg(feature = "mcp")]
pub fn serve() -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::model::ServerInfo;
    use rmcp::schemars;
    use rmcp::{ServerHandler, tool, tool_handler, tool_router, transport::stdio};

    use crate::canonical::Agent;

    #[derive(Clone)]
    struct BatonServer;

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct ListSessionsParams {
        /// Filter to a single agent ("claude-code", "opencode", ...). Omit to scan all.
        #[serde(default)]
        agent: Option<String>,
    }

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct ConvertSessionParams {
        /// Source agent name (e.g. "claude-code").
        from: String,
        /// Target agent name (e.g. "opencode").
        to: String,
        /// Path to the source session file.
        input: String,
        /// Where to write the converted session. Optional.
        #[serde(default)]
        output: Option<String>,
    }

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct ImportToTargetParams {
        /// Target agent name (e.g. "opencode").
        to: String,
        /// Path to the already-converted session file.
        file: String,
    }

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct DetectFormatParams {
        /// Path to the session file or directory to sniff.
        path: String,
    }

    #[tool_router]
    impl BatonServer {
        #[tool(description = "List coding-agent sessions across all detected agents, or filter by one. Returns one session per line: <agent>\\t<id>\\t<path>.")]
        fn list_sessions(
            &self,
            Parameters(p): Parameters<ListSessionsParams>,
        ) -> String {
            let agent = p.agent.as_deref().and_then(Agent::parse);
            let agents = match agent {
                Some(a) => vec![a],
                None => crate::formats::ALL_AGENTS.to_vec(),
            };
            let mut out = String::new();
            for a in agents {
                for r in crate::formats::list(a) {
                    out.push_str(&format!("{}\t{}\t{}\n", a, r.id, r.path.display()));
                }
            }
            if out.is_empty() {
                out.push_str("no sessions found");
            }
            out
        }

        #[tool(description = "Convert a session from one agent format to another and write it to disk. Returns 'ok' or 'error: ...'.")]
        fn convert_session(
            &self,
            Parameters(p): Parameters<ConvertSessionParams>,
        ) -> String {
            let from = match Agent::parse(&p.from) {
                Some(a) => a,
                None => return format!("unknown source agent: {}", p.from),
            };
            let to = match Agent::parse(&p.to) {
                Some(a) => a,
                None => return format!("unknown target agent: {}", p.to),
            };
            match crate::convert::convert(
                from,
                to,
                std::path::Path::new(&p.input),
                p.output.as_deref().map(std::path::Path::new),
            ) {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("error: {e:#}"),
            }
        }

        #[tool(description = "Convert a session and run the target agent's own import command (e.g. `opencode import`). Returns 'imported' or 'error: ...'.")]
        fn import_to_target(
            &self,
            Parameters(p): Parameters<ImportToTargetParams>,
        ) -> String {
            let to = match Agent::parse(&p.to) {
                Some(a) => a,
                None => return format!("unknown target agent: {}", p.to),
            };
            match crate::convert::import_to_target(to, std::path::Path::new(&p.file)) {
                Ok(()) => "imported".to_string(),
                Err(e) => format!("error: {e:#}"),
            }
        }

        #[tool(description = "Sniff a session file/dir and report which coding agent produced it.")]
        fn detect_format(
            &self,
            Parameters(p): Parameters<DetectFormatParams>,
        ) -> String {
            let path = std::path::Path::new(&p.path);
            let agent = crate::detect::detect_at_path(path);
            agent.to_string()
        }
    }

    #[tool_handler]
    impl ServerHandler for BatonServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
            .with_server_info(rmcp::model::Implementation::new(
                "baton",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Pass the baton between coding agents. Tools: list_sessions, convert_session, import_to_target, detect_format.",
            )
        }
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let service = BatonServer.serve(stdio()).await?;
        service.waiting().await?;
        Ok::<(), anyhow::Error>(())
    })
}
