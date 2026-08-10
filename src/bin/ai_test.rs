// ai_test — sanity check for the configured AI provider's connection,
// nothing more.
//
// Usage:
//   export AI_PROVIDER=openai        # or anthropic / gemini / qwen / custom
//   export OPENAI_API_KEY=sk-...     # (or ANTHROPIC_API_KEY / GEMINI_API_KEY / DASHSCOPE_API_KEY)
//   cargo run --bin ai_test -- "what CPU architecture is a Harvard architecture typically paired with?"
//
// A working reply here is the ONLY thing this binary proves: that the
// provider, key, endpoint, and request shape are correct. It says nothing
// yet about how the decompiler will use the model. If AI_PROVIDER isn't
// set, it falls back to whatever provider was last chosen in the GUI's
// Settings dialog, then to Qwen.

use mini_decompiler::ai::AiClient;
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.trim().is_empty() {
        "Reply with exactly: connection ok".to_string()
    } else {
        prompt
    };

    let client = match AiClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("setup error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("--- provider: {} ---", client.provider().display_name());

    let system = "You are a terse test responder. Keep replies to one sentence.";

    match client.ask(system, &prompt) {
        Ok(reply) => {
            println!("--- reply ---");
            println!("{}", reply.text);
            if let (Some(p), Some(c)) = (reply.prompt_tokens, reply.completion_tokens) {
                println!("--- usage: {p} prompt tokens, {c} completion tokens ---");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("request error: {e}");
            ExitCode::FAILURE
        }
    }
}
