// ai.rs — thin client for AI-assisted naming, speaking to whichever
// provider the user has configured: OpenAI (ChatGPT), Anthropic (Claude),
// Google (Gemini), Alibaba's Qwen (DashScope), or any other
// OpenAI-compatible endpoint ("custom" — local models, other hosts, etc).
//
// This is Stage 3 plumbing only: "can we ask the model a question and get
// text back." It does NOT decide when to call the model, how to prompt it,
// or how to parse structured answers — that's the orchestrator's job
// (`rename.rs`), built on top of this once the connection itself is proven
// out.
//
// Three of the five providers (OpenAI, Qwen/DashScope, and any "custom"
// endpoint) speak the same OpenAI chat-completions wire format, so those
// share one request/response path. Anthropic and Gemini have their own
// wire formats and get their own request builders below.
//
// Which provider is active, and where each provider's key/model/base-url
// comes from, is resolved in this order: the provider-specific env var
// (e.g. OPENAI_API_KEY) wins if set; otherwise we fall back to whatever
// was saved via the GUI's Settings dialog (see `crate::config`).

use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::time::Duration;

// ------------------------------------------------------------- providers

/// Which AI backend to talk to. `Custom` is any other OpenAI-compatible
/// endpoint (self-hosted models, other hosts) — same wire format as
/// OpenAI/Qwen, just with a user-supplied base URL and model name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OpenAI,
    Anthropic,
    Gemini,
    Qwen,
    Custom,
}

impl Provider {
    /// All providers, in the order they should appear in UI pickers.
    pub const ALL: [Provider; 5] =
        [Provider::OpenAI, Provider::Anthropic, Provider::Gemini, Provider::Qwen, Provider::Custom];

    /// Stable lowercase key used in both env-var lookups and the config
    /// file (`crate::config`) — e.g. "openai", "anthropic".
    pub fn key(&self) -> &'static str {
        match self {
            Provider::OpenAI => "openai",
            Provider::Anthropic => "anthropic",
            Provider::Gemini => "gemini",
            Provider::Qwen => "qwen",
            Provider::Custom => "custom",
        }
    }

    /// Human-readable name for menus and dialogs.
    pub fn display_name(&self) -> &'static str {
        match self {
            Provider::OpenAI => "OpenAI (ChatGPT)",
            Provider::Anthropic => "Anthropic (Claude)",
            Provider::Gemini => "Google (Gemini)",
            Provider::Qwen => "Alibaba (Qwen / DashScope)",
            Provider::Custom => "Custom (OpenAI-compatible)",
        }
    }

    /// Parses a provider key case-insensitively, also accepting a few
    /// obvious aliases so `AI_PROVIDER=chatgpt` or `=claude` do the
    /// friendly thing instead of erroring.
    pub fn parse(s: &str) -> Option<Provider> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" | "chatgpt" | "gpt" => Some(Provider::OpenAI),
            "anthropic" | "claude" => Some(Provider::Anthropic),
            "gemini" | "google" => Some(Provider::Gemini),
            "qwen" | "dashscope" | "alibaba" => Some(Provider::Qwen),
            "custom" | "other" => Some(Provider::Custom),
            _ => None,
        }
    }

    /// Which provider is active: `AI_PROVIDER` env var if set and
    /// recognised, else the provider saved via the Settings dialog, else
    /// Qwen (the original, pre-multi-provider default).
    pub fn active() -> Provider {
        if let Ok(v) = env::var("AI_PROVIDER") {
            if let Some(p) = Provider::parse(&v) {
                return p;
            }
        }
        if let Some(saved) = crate::config::load_active_provider() {
            if let Some(p) = Provider::parse(&saved) {
                return p;
            }
        }
        Provider::Qwen
    }

    fn env_api_key_var(&self) -> &'static str {
        match self {
            Provider::OpenAI => "OPENAI_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Gemini => "GEMINI_API_KEY",
            Provider::Qwen => "DASHSCOPE_API_KEY",
            Provider::Custom => "CUSTOM_API_KEY",
        }
    }

    fn env_base_url_var(&self) -> &'static str {
        match self {
            Provider::OpenAI => "OPENAI_BASE_URL",
            Provider::Anthropic => "ANTHROPIC_BASE_URL",
            Provider::Gemini => "GEMINI_BASE_URL",
            Provider::Qwen => "DASHSCOPE_BASE_URL",
            Provider::Custom => "CUSTOM_BASE_URL",
        }
    }

    fn env_model_var(&self) -> &'static str {
        match self {
            Provider::OpenAI => "OPENAI_MODEL",
            Provider::Anthropic => "ANTHROPIC_MODEL",
            Provider::Gemini => "GEMINI_MODEL",
            Provider::Qwen => "QWEN_MODEL",
            Provider::Custom => "CUSTOM_MODEL",
        }
    }

    /// Built-in default base URL. `Custom` has none — it must come from
    /// the env var or config, since "custom" only means anything with a
    /// user-supplied endpoint.
    fn default_base_url(&self) -> Option<&'static str> {
        match self {
            Provider::OpenAI => Some("https://api.openai.com/v1"),
            Provider::Anthropic => Some("https://api.anthropic.com/v1"),
            Provider::Gemini => Some("https://generativelanguage.googleapis.com/v1beta"),
            Provider::Qwen => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
            Provider::Custom => None,
        }
    }

    /// Built-in default model. `Custom` has none, same reasoning as
    /// `default_base_url`.
    fn default_model(&self) -> Option<&'static str> {
        match self {
            // Small/cheap, widely-available default for each hosted
            // provider; the whole point of the per-provider *_MODEL env
            // var and the Settings dialog's model field is to let anyone
            // point this at something newer without a rebuild — provider
            // lineups move fast enough that these will go stale too, so
            // treat them as a starting point, not gospel.
            Provider::OpenAI => Some("gpt-5-mini"),
            Provider::Anthropic => Some("claude-sonnet-4-6"),
            Provider::Gemini => Some("gemini-3.6-flash"),
            Provider::Qwen => Some("qwen3.8-max"),
            Provider::Custom => None,
        }
    }

    /// Whether this provider/model combination is known to accept the
    /// `reasoning_effort` chat-completions field. Qwen3.8-Max always does
    /// (see `ask_openai_compatible`); on OpenAI it's only the reasoning
    /// model families (o-series, gpt-5+) — sending it to a non-reasoning
    /// model (gpt-4.1, gpt-4o, ...) is a 400. `Custom` endpoints vary too
    /// much to guess, so it's never sent there.
    fn supports_reasoning_effort(&self, model: &str) -> bool {
        match self {
            Provider::Qwen => true,
            Provider::OpenAI => {
                let m = model.to_ascii_lowercase();
                m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.contains("gpt-5")
            }
            Provider::Anthropic | Provider::Gemini | Provider::Custom => false,
        }
    }

    /// Whether this provider's wire format is OpenAI's chat-completions
    /// shape (as opposed to Anthropic's or Gemini's own formats).
    fn is_openai_compatible(&self) -> bool {
        matches!(self, Provider::OpenAI | Provider::Qwen | Provider::Custom)
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

// ------------------------------------------------------------------ error

#[derive(Debug)]
pub enum AiError {
    MissingApiKey(Provider),
    MissingBaseUrl(Provider),
    MissingModel(Provider),
    Request(String),
    Api { status: u16, body: String },
    EmptyResponse,
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiError::MissingApiKey(p) => write!(
                f,
                "no {} API key found (set {}, or add one via File > AI Settings…)",
                p.display_name(),
                p.env_api_key_var()
            ),
            AiError::MissingBaseUrl(p) => write!(
                f,
                "no base URL set for {} (set {}, or add one via File > AI Settings…)",
                p.display_name(),
                p.env_base_url_var()
            ),
            AiError::MissingModel(p) => write!(
                f,
                "no model set for {} (set {}, or add one via File > AI Settings…)",
                p.display_name(),
                p.env_model_var()
            ),
            AiError::Request(e) => write!(f, "request failed: {e}"),
            AiError::Api { status, body } => write!(f, "API returned HTTP {status}: {body}"),
            AiError::EmptyResponse => write!(f, "the model returned no reply"),
        }
    }
}

impl std::error::Error for AiError {}

// ------------------------------------------------------------------ client

pub struct AiClient {
    provider: Provider,
    api_key: String,
    base_url: String,
    model: String,
    agent: ureq::Agent,
}

impl AiClient {
    /// Builds a client for whichever provider is active (see
    /// `Provider::active`), reading that provider's key/base-url/model
    /// from its env var first, then the saved config, then the built-in
    /// default.
    pub fn from_env() -> Result<Self, AiError> {
        Self::for_provider(Provider::active())
    }

    /// Builds a client for a specific provider, ignoring `AI_PROVIDER` /
    /// the saved "active provider" setting. Useful for the Settings
    /// dialog's "test connection" action, or for callers that want a
    /// particular provider regardless of what's currently active.
    pub fn for_provider(provider: Provider) -> Result<Self, AiError> {
        let api_key = env::var(provider.env_api_key_var())
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| crate::config::load_api_key(provider.key()))
            .ok_or(AiError::MissingApiKey(provider))?;

        let base_url = env::var(provider.env_base_url_var())
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| crate::config::load_base_url(provider.key()))
            .or_else(|| provider.default_base_url().map(str::to_string))
            .ok_or(AiError::MissingBaseUrl(provider))?;

        let model = env::var(provider.env_model_var())
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| crate::config::load_model(provider.key()))
            .or_else(|| provider.default_model().map(str::to_string))
            .ok_or(AiError::MissingModel(provider))?;

        let agent = ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(
                native_tls::TlsConnector::new().map_err(|e| AiError::Request(e.to_string()))?,
            ))
            .timeout(Duration::from_secs(120))
            .build();

        Ok(Self { provider, api_key, base_url, model, agent })
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Send a single system+user turn, no conversation history, no tools.
    /// Returns the model's text reply.
    pub fn ask(&self, system: &str, user: &str) -> Result<AiReply, AiError> {
        self.ask_with_effort(system, user, "low")
    }

    /// Same as `ask`, but with a reasoning-effort hint for callers that
    /// have an actual reason to raise it above "low" (e.g. a naming task
    /// with more code to weigh than a one-line lookup). Only Qwen/DashScope
    /// currently exposes this knob over the API; other providers accept
    /// the parameter but silently ignore it here.
    pub fn ask_with_effort(
        &self,
        system: &str,
        user: &str,
        reasoning_effort: &str,
    ) -> Result<AiReply, AiError> {
        match self.provider {
            _ if self.provider.is_openai_compatible() => {
                self.ask_openai_compatible(system, user, reasoning_effort)
            }
            Provider::Anthropic => self.ask_anthropic(system, user),
            Provider::Gemini => self.ask_gemini(system, user),
            Provider::OpenAI | Provider::Qwen | Provider::Custom => unreachable!(),
        }
    }

    // ------------------------------------------------ OpenAI-compatible
    // (OpenAI itself, Qwen/DashScope's compatible-mode endpoint, and any
    // "custom" OpenAI-compatible endpoint all share this path.)

    fn ask_openai_compatible(
        &self,
        system: &str,
        user: &str,
        reasoning_effort: &str,
    ) -> Result<AiReply, AiError> {
        // Thinking tokens bill as output tokens on Qwen3.8-Max (which
        // defaults to reasoning_effort "xhigh"), so this is pinned
        // explicitly there. Other OpenAI-compatible backends don't all
        // support the field — sending it to a model that doesn't expect
        // it is a 400 — so it's only sent where `supports_reasoning_effort`
        // says the provider/model combination is known to accept it.
        let reasoning_effort = if self.provider.supports_reasoning_effort(&self.model) {
            Some(reasoning_effort)
        } else {
            None
        };

        let req_body = OpenAiChatRequest {
            model: &self.model,
            messages: vec![
                OpenAiChatMessage { role: "system", content: system },
                OpenAiChatMessage { role: "user", content: user },
            ],
            reasoning_effort,
        };

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let result = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(&req_body);

        let raw = Self::read_body(result)?;

        let parsed: OpenAiChatResponse = serde_json::from_str(&raw).map_err(|e| AiError::Api {
            status: 200,
            body: format!("failed to parse response ({e}): {raw}"),
        })?;

        let choice = parsed.choices.into_iter().next().ok_or(AiError::EmptyResponse)?;

        Ok(AiReply {
            text: choice.message.content,
            prompt_tokens: parsed.usage.as_ref().map(|u| u.prompt_tokens),
            completion_tokens: parsed.usage.as_ref().map(|u| u.completion_tokens),
        })
    }

    // ------------------------------------------------------------ Anthropic

    fn ask_anthropic(&self, system: &str, user: &str) -> Result<AiReply, AiError> {
        let req_body = AnthropicRequest {
            model: &self.model,
            max_tokens: 4096,
            system,
            messages: vec![AnthropicMessage { role: "user", content: user }],
        };

        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));

        let result = self
            .agent
            .post(&url)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .send_json(&req_body);

        let raw = Self::read_body(result)?;

        let parsed: AnthropicResponse = serde_json::from_str(&raw).map_err(|e| AiError::Api {
            status: 200,
            body: format!("failed to parse response ({e}): {raw}"),
        })?;

        let text = parsed
            .content
            .into_iter()
            .find(|b| b.block_type == "text")
            .map(|b| b.text)
            .ok_or(AiError::EmptyResponse)?;

        Ok(AiReply {
            text,
            prompt_tokens: parsed.usage.as_ref().map(|u| u.input_tokens),
            completion_tokens: parsed.usage.as_ref().map(|u| u.output_tokens),
        })
    }

    // --------------------------------------------------------------- Gemini

    fn ask_gemini(&self, system: &str, user: &str) -> Result<AiReply, AiError> {
        let req_body = GeminiRequest {
            system_instruction: GeminiContent {
                role: None,
                parts: vec![GeminiPart { text: system }],
            },
            contents: vec![GeminiContent {
                role: Some("user"),
                parts: vec![GeminiPart { text: user }],
            }],
        };

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url.trim_end_matches('/'),
            self.model,
            self.api_key
        );

        let result = self.agent.post(&url).send_json(&req_body);

        let raw = Self::read_body(result)?;

        let parsed: GeminiResponse = serde_json::from_str(&raw).map_err(|e| AiError::Api {
            status: 200,
            body: format!("failed to parse response ({e}): {raw}"),
        })?;

        let candidate = parsed.candidates.into_iter().next().ok_or(AiError::EmptyResponse)?;
        let part = candidate.content.parts.into_iter().next().ok_or(AiError::EmptyResponse)?;

        Ok(AiReply {
            text: part.text,
            prompt_tokens: parsed.usage_metadata.as_ref().map(|u| u.prompt_token_count),
            completion_tokens: parsed.usage_metadata.as_ref().and_then(|u| u.candidates_token_count),
        })
    }

    // ------------------------------------------------------------ shared

    fn read_body(result: Result<ureq::Response, ureq::Error>) -> Result<String, AiError> {
        match result {
            Ok(resp) => resp.into_string().map_err(|e| AiError::Request(e.to_string())),
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_else(|_| "<unreadable body>".to_string());
                Err(AiError::Api { status, body })
            }
            Err(ureq::Error::Transport(t)) => Err(AiError::Request(t.to_string())),
        }
    }
}

pub struct AiReply {
    pub text: String,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

// -------------------------------------------------- wire formats: OpenAI

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

#[derive(Serialize)]
struct OpenAiChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
}

#[derive(Deserialize)]
struct OpenAiResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// ----------------------------------------------- wire formats: Anthropic

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<AnthropicMessage<'a>>,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicBlock>,
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

// -------------------------------------------------- wire formats: Gemini

#[derive(Serialize)]
struct GeminiRequest<'a> {
    system_instruction: GeminiContent<'a>,
    contents: Vec<GeminiContent<'a>>,
}

#[derive(Serialize)]
struct GeminiContent<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'a str>,
    parts: Vec<GeminiPart<'a>>,
}

#[derive(Serialize)]
struct GeminiPart<'a> {
    text: &'a str,
}

#[derive(Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiResponseContent,
}

#[derive(Deserialize)]
struct GeminiResponseContent {
    #[serde(default)]
    parts: Vec<GeminiResponsePart>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct GeminiUsage {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u32>,
}
