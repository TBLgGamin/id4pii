use std::collections::HashSet;

use super::ca;

const DEFAULT_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "chatgpt.com",
    "chat.openai.com",
    "claude.ai",
    "api.x.ai",
    "generativelanguage.googleapis.com",
    "api.cohere.com",
    "api.mistral.ai",
    "api.groq.com",
    "openrouter.ai",
    "api.deepseek.com",
];

#[derive(Debug)]
pub(crate) struct HostMatcher {
    hosts: HashSet<String>,
}

impl HostMatcher {
    pub(crate) fn load() -> Self {
        let mut hosts: HashSet<String> = DEFAULT_HOSTS
            .iter()
            .map(|host| (*host).to_string())
            .collect();
        if let Ok(dir) = ca::config_dir()
            && let Ok(text) = std::fs::read_to_string(dir.join("hosts.txt"))
        {
            for line in text.lines() {
                let entry = line.trim();
                if !entry.is_empty() && !entry.starts_with('#') {
                    hosts.insert(entry.to_ascii_lowercase());
                }
            }
        }
        Self { hosts }
    }

    pub(crate) fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        let host = host.strip_prefix("www.").unwrap_or(&host);
        self.hosts
            .iter()
            .any(|known| host == known || host.ends_with(&format!(".{known}")))
    }
}
