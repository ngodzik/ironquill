use ironquill_core::{Agent, Delegate, DelegateEvent, DelegateReply, DelegateRequest};

use crate::claude_code::ClaudeCode;
use crate::codex::Codex;
use crate::error::LlmError;

/// Every agent a task can be handed to, each request going to the one it
/// names.
#[derive(Debug, Clone)]
pub struct Agents {
    /// Claude Code.
    pub claude: ClaudeCode,
    /// Codex.
    pub codex: Codex,
}

impl Agents {
    /// The agents as found on the `PATH`. One that is not installed is
    /// still called by name, so that picking it fails with a message saying
    /// so rather than at startup.
    pub fn find() -> Self {
        Self {
            claude: ClaudeCode::find().unwrap_or_else(|| ClaudeCode::new("claude")),
            codex: Codex::find().unwrap_or_else(|| Codex::new("codex")),
        }
    }
}

impl Delegate for Agents {
    type Error = LlmError;

    async fn run(
        &self,
        request: &DelegateRequest,
        on_event: &mut (dyn FnMut(DelegateEvent) + Send),
    ) -> Result<DelegateReply, LlmError> {
        match request.agent {
            Agent::ClaudeCode => self.claude.run(request, on_event).await,
            Agent::Codex => self.codex.run(request, on_event).await,
        }
    }
}
