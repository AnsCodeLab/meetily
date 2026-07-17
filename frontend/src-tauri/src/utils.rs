pub fn format_timestamp(seconds: f64) -> String {
    let total_seconds = seconds as u64;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, secs)
}

/// Parse the first complete JSON value from `text`, ignoring any trailing bytes.
///
/// Some OpenAI-compatible servers (notably certain llama.cpp/llama-server builds)
/// append a stray SSE terminator like ` data: [DONE]` after a perfectly valid,
/// non-streaming JSON response body. `serde_json::from_str` rejects that as
/// "trailing characters" because it also requires the input to be fully consumed.
/// This only deserializes the leading JSON value and stops, which is what every
/// OpenAI-compatible client actually needs.
pub fn parse_json_prefix<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    serde::de::Deserialize::deserialize(&mut deserializer)
}

/// Opens macOS System Settings to a specific privacy preference pane
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn open_system_settings(preference_pane: String) -> Result<(), String> {
    use std::process::Command;

    // Construct the URL for System Settings
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{}", preference_pane);

    // Use the 'open' command on macOS to open the URL
    Command::new("open")
        .arg(&url)
        .spawn()
        .map_err(|e| format!("Failed to open system settings: {}", e))?;

    Ok(())
} 

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_prefix_ignores_trailing_sse_terminator() {
        // Exact shape of a real response from a llama.cpp/llama-server-based
        // OpenAI-compatible endpoint: a valid JSON object immediately followed
        // by a stray ` data: [DONE]` SSE terminator, even for a non-streaming
        // request. This must not be treated as invalid JSON.
        let response = r#"{"choices":[{"finish_reason":"length","index":0,"message":{"role":"assistant","content":"","reasoning_content":"Thinking Process:\n\n"}}],"created":1784258328,"model":"Qwen3.5-4B-Q4_K_M.gguf","object":"chat.completion"} data: [DONE]"#;

        let value: serde_json::Value =
            parse_json_prefix(response).expect("leading JSON object must parse despite trailing garbage");

        assert_eq!(
            value["choices"][0]["message"]["role"],
            serde_json::json!("assistant")
        );
    }

    #[test]
    fn parse_json_prefix_still_rejects_genuinely_invalid_json() {
        let result = parse_json_prefix::<serde_json::Value>("{not valid json");
        assert!(result.is_err());
    }
}