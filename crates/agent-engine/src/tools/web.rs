use super::Tool;
use crate::mcp::EndpointValidator;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// HTML to Markdown Converter
// ---------------------------------------------------------------------------

pub fn html_to_markdown(html: &str) -> String {
    // 1. Remove scripts, styles, noscript, and SVGs completely
    let re_remove =
        Regex::new(r"(?is)<(?:script|style|noscript|svg)[\s\S]*?</(?:script|style|noscript|svg)>")
            .unwrap();
    let mut text = re_remove.replace_all(html, "").to_string();

    // 2. Extract title tag if present
    let re_title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap();
    let title = if let Some(cap) = re_title.captures(&text) {
        let t = cap[1].trim();
        if !t.is_empty() {
            format!("# {t}\n\n")
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    text = re_title.replace_all(&text, "").to_string();

    // Code blocks: <pre><code>...</code></pre> or <pre>...</pre>
    let re_pre_code = Regex::new(r"(?is)<pre[^>]*><code[^>]*>(.*?)</code></pre>").unwrap();
    text = re_pre_code
        .replace_all(&text, "\n\n```\n$1\n```\n\n")
        .to_string();

    let re_pre = Regex::new(r"(?is)<pre[^>]*>(.*?)</pre>").unwrap();
    text = re_pre
        .replace_all(&text, "\n\n```\n$1\n```\n\n")
        .to_string();

    // Inline code: <code>...</code>
    let re_code = Regex::new(r"(?is)<code[^>]*>(.*?)</code>").unwrap();
    text = re_code.replace_all(&text, "`$1`").to_string();

    // Headings <h1> - <h6>
    let re_h1 = Regex::new(r"(?is)<h1[^>]*>(.*?)</h1>").unwrap();
    text = re_h1.replace_all(&text, "\n\n# $1\n\n").to_string();
    let re_h2 = Regex::new(r"(?is)<h2[^>]*>(.*?)</h2>").unwrap();
    text = re_h2.replace_all(&text, "\n\n## $1\n\n").to_string();
    let re_h3 = Regex::new(r"(?is)<h3[^>]*>(.*?)</h3>").unwrap();
    text = re_h3.replace_all(&text, "\n\n### $1\n\n").to_string();
    let re_h4 = Regex::new(r"(?is)<h4[^>]*>(.*?)</h4>").unwrap();
    text = re_h4.replace_all(&text, "\n\n#### $1\n\n").to_string();
    let re_h5 = Regex::new(r"(?is)<h5[^>]*>(.*?)</h5>").unwrap();
    text = re_h5.replace_all(&text, "\n\n##### $1\n\n").to_string();
    let re_h6 = Regex::new(r"(?is)<h6[^>]*>(.*?)</h6>").unwrap();
    text = re_h6.replace_all(&text, "\n\n###### $1\n\n").to_string();

    // Links: <a href="url">text</a>
    let re_link = Regex::new(r#"(?is)<a\s+[^>]*href=["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap();
    text = re_link.replace_all(&text, "[$2]($1)").to_string();

    // Images: <img src="url" alt="alt" />
    let re_img =
        Regex::new(r#"(?is)<img\s+[^>]*src=["']([^"']+)["'][^>]*alt=["']([^"']*)["'][^>]*>"#)
            .unwrap();
    text = re_img.replace_all(&text, "![$2]($1)").to_string();

    // Bold: <b>, <strong>
    let re_strong = Regex::new(r"(?is)<strong(?:\s+[^>]*)?>(.*?)</strong>").unwrap();
    text = re_strong.replace_all(&text, "**$1**").to_string();
    let re_b = Regex::new(r"(?is)<b(?:\s+[^>]*)?>(.*?)</b>").unwrap();
    text = re_b.replace_all(&text, "**$1**").to_string();

    // Italic: <i>, <em>
    let re_em = Regex::new(r"(?is)<em(?:\s+[^>]*)?>(.*?)</em>").unwrap();
    text = re_em.replace_all(&text, "*$1*").to_string();
    let re_i = Regex::new(r"(?is)<i(?:\s+[^>]*)?>(.*?)</i>").unwrap();
    text = re_i.replace_all(&text, "*$1*").to_string();

    // Blockquote: <blockquote>...</blockquote>
    let re_quote = Regex::new(r"(?is)<blockquote[^>]*>(.*?)</blockquote>").unwrap();
    text = re_quote.replace_all(&text, "\n> $1\n").to_string();

    // List items: <li>...</li>
    let re_li = Regex::new(r"(?is)<li[^>]*>(.*?)</li>").unwrap();
    text = re_li.replace_all(&text, "\n* $1").to_string();

    // Paragraphs and breaks
    let re_p = Regex::new(r"(?is)<p[^>]*>(.*?)</p>").unwrap();
    text = re_p.replace_all(&text, "\n\n$1\n\n").to_string();

    let re_br = Regex::new(r"(?is)<br\s*/?>").unwrap();
    text = re_br.replace_all(&text, "\n").to_string();

    let re_hr = Regex::new(r"(?is)<hr\s*/?>").unwrap();
    text = re_hr.replace_all(&text, "\n\n---\n\n").to_string();

    // Remove remaining HTML tags
    let re_tags = Regex::new(r"<[^>]+>").unwrap();
    text = re_tags.replace_all(&text, "").to_string();

    // Decode HTML entities
    let text = decode_html_entities(&text);

    // Normalize whitespace: collapse 3+ newlines to 2, trim lines
    let re_newlines = Regex::new(r"\n{3,}").unwrap();
    let cleaned = re_newlines.replace_all(&text, "\n\n");

    let mut result = title;
    result.push_str(cleaned.trim());
    result
}

fn decode_html_entities(input: &str) -> String {
    let mut text = input
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&copy;", "©")
        .replace("&reg;", "®")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–");

    // Decode decimal numeric character references: &#(\d+);
    let re_decimal = Regex::new(r"&#(\d+);").unwrap();
    text = re_decimal
        .replace_all(&text, |caps: &regex::Captures| {
            if let Ok(code) = caps[1].parse::<u32>() {
                if let Some(ch) = char::from_u32(code) {
                    return ch.to_string();
                }
            }
            caps[0].to_string()
        })
        .to_string();

    text
}

// ---------------------------------------------------------------------------
// WebFetch Tool
// ---------------------------------------------------------------------------

pub struct WebFetchTool;

impl Default for WebFetchTool {
    fn default() -> Self {
        Self
    }
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }

    pub async fn fetch_url(raw_url: &str, validator: &EndpointValidator) -> Result<String, String> {
        // 1. Validate URL to prevent SSRF and illegal schemes
        let validated_url = validator
            .validate_url(raw_url)
            .map_err(|e| format!("URL validation failed: {e}"))?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Ferric/0.1.0")
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        // 2. Perform HTTP GET request
        let response = client
            .get(validated_url.as_str())
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP error {status} for URL '{raw_url}'"));
        }

        let is_html = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("text/html") || ct.contains("application/xhtml+xml"))
            .unwrap_or(false);

        let body = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response body: {e}"))?;

        // 3. Extract markdown if HTML
        let output = if is_html || body.contains("<html") || body.contains("<!DOCTYPE") {
            html_to_markdown(&body)
        } else {
            body
        };

        // Truncate if response is excessively large
        if output.len() > 100_000 {
            let truncated = &output[..100_000];
            Ok(format!(
                "{truncated}\n\n... [Response truncated; {} total bytes]",
                output.len()
            ))
        } else {
            Ok(output)
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "WebFetch".into(),
            description: "Fetch web content via HTTP GET, validating the URL and converting HTML to markdown."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The HTTP or HTTPS URL to fetch"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let raw_url = input["url"].as_str().ok_or("Missing 'url' argument")?;
        let validator = EndpointValidator::default();
        Self::fetch_url(raw_url, &validator).await
    }
}

// ---------------------------------------------------------------------------
// WebSearch Tool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str) -> Result<Vec<SearchResult>, String>;
}

/// Default search provider querying DuckDuckGo HTML endpoint
pub struct DuckDuckGoSearchProvider {
    client: reqwest::Client,
}

impl Default for DuckDuckGoSearchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DuckDuckGoSearchProvider {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:109.0) Gecko/20100101 Firefox/119.0",
            )
            .build()
            .unwrap_or_default();

        Self { client }
    }
}

#[async_trait]
impl SearchProvider for DuckDuckGoSearchProvider {
    async fn search(&self, query: &str) -> Result<Vec<SearchResult>, String> {
        let resp = self
            .client
            .post("https://html.duckduckgo.com/html/")
            .form(&[("q", query)])
            .send()
            .await
            .map_err(|e| format!("DuckDuckGo search request failed: {e}"))?;

        let html = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read search response: {e}"))?;

        // Parse search results from DuckDuckGo HTML
        let results = parse_duckduckgo_results(&html);
        Ok(results)
    }
}

fn parse_duckduckgo_results(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let re_result = Regex::new(
        r#"(?is)<a[^>]+class="result__url"[^>]+href=["']([^"']+)["'][^>]*>(.*?)</a>[\s\S]*?<a[^>]+class="result__snippet"[^>]*>(.*?)</a>"#
    ).unwrap();

    for cap in re_result.captures_iter(html) {
        let raw_url = cap[1].trim();
        let title = html_to_markdown(&cap[2]).trim().to_string();
        let snippet = html_to_markdown(&cap[3]).trim().to_string();

        let clean_url = if raw_url.starts_with("//duckduckgo.com/l/?uddg=") {
            if let Some(pos) = raw_url.find("uddg=") {
                let encoded = &raw_url[pos + 5..].split('&').next().unwrap_or("");
                urlencoding_decode(encoded)
            } else {
                raw_url.to_string()
            }
        } else {
            raw_url.to_string()
        };

        if !clean_url.is_empty() {
            results.push(SearchResult {
                title: if title.is_empty() {
                    clean_url.clone()
                } else {
                    title
                },
                url: clean_url,
                snippet,
            });
        }
    }

    results
}

fn urlencoding_decode(s: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().unwrap_or(' ');
            let h2 = chars.next().unwrap_or(' ');
            if let Ok(b) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                bytes.push(b);
            }
        } else if c == '+' {
            bytes.push(b' ');
        } else {
            bytes.push(c as u8);
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}

static SEARCH_PROVIDER_OVERRIDE: RwLock<Option<Arc<dyn SearchProvider>>> = RwLock::new(None);

pub fn set_search_provider_override(provider: Option<Arc<dyn SearchProvider>>) {
    let mut lock = SEARCH_PROVIDER_OVERRIDE.write().unwrap();
    *lock = provider;
}

pub struct WebSearchTool;

impl Default for WebSearchTool {
    fn default() -> Self {
        Self
    }
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "WebSearch".into(),
            description:
                "Search the web for queries, filtering results by allowed and blocked domains."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query string"
                    },
                    "allowed_domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of domains to filter results by (e.g. ['docs.rs', 'github.com'])"
                    },
                    "blocked_domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of domains to exclude from results"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let query = input["query"].as_str().ok_or("Missing 'query' argument")?;

        let allowed_domains: Vec<String> = input["allowed_domains"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();

        let blocked_domains: Vec<String> = input["blocked_domains"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();

        let provider: Arc<dyn SearchProvider> = {
            let lock = SEARCH_PROVIDER_OVERRIDE.read().unwrap();
            match &*lock {
                Some(p) => p.clone(),
                None => Arc::new(DuckDuckGoSearchProvider::new()),
            }
        };

        let raw_results = provider.search(query).await?;

        // Filter results by domain
        let filtered: Vec<SearchResult> = raw_results
            .into_iter()
            .filter(|res| {
                let parsed_url = match url::Url::parse(&res.url) {
                    Ok(u) => u,
                    Err(_) => return false,
                };
                let host = match parsed_url.host_str() {
                    Some(h) => h.to_lowercase(),
                    None => return false,
                };

                // Check blocked domains
                for blocked in &blocked_domains {
                    if host == *blocked || host.ends_with(&format!(".{blocked}")) {
                        return false;
                    }
                }

                // Check allowed domains
                if !allowed_domains.is_empty() {
                    let mut is_allowed = false;
                    for allowed in &allowed_domains {
                        if host == *allowed || host.ends_with(&format!(".{allowed}")) {
                            is_allowed = true;
                            break;
                        }
                    }
                    if !is_allowed {
                        return false;
                    }
                }

                true
            })
            .collect();

        if filtered.is_empty() {
            return Ok(format!(
                "No search results found for query '{query}' matching domain criteria."
            ));
        }

        let mut output = format!(
            "Search results for '{}' ({} found):\n\n",
            query,
            filtered.len()
        );
        for (idx, item) in filtered.iter().enumerate() {
            output.push_str(&format!(
                "{}. [{}]({})\n   {}\n\n",
                idx + 1,
                item.title,
                item.url,
                item.snippet
            ));
        }

        Ok(output.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_markdown_comprehensive() {
        let sample_html = r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>Test Page Title</title>
            <style>body { color: red; }</style>
            <script>alert("hello");</script>
        </head>
        <body>
            <h1>Main Heading</h1>
            <p>This is a <b>bold</b> and <i>italic</i> paragraph with <a href="https://example.com">a link</a>.</p>
            <h2>Code Section</h2>
            <pre><code>fn main() {
    println!("Hello, world!");
}</code></pre>
            <p>Inline <code>x = 42</code> check.</p>
            <ul>
                <li>Item 1</li>
                <li>Item 2</li>
            </ul>
            <blockquote>A wise quote &amp; wisdom.</blockquote>
        </body>
        </html>
        "#;

        let md = html_to_markdown(sample_html);

        assert!(md.contains("# Test Page Title"));
        assert!(md.contains("# Main Heading"));
        assert!(md.contains("**bold**"));
        assert!(md.contains("*italic*"));
        assert!(md.contains("[a link](https://example.com)"));
        assert!(md.contains("```\nfn main() {"));
        assert!(md.contains("`x = 42`"));
        assert!(md.contains("* Item 1"));
        assert!(md.contains("> A wise quote & wisdom."));
        // Scripts and styles removed
        assert!(!md.contains("alert"));
        assert!(!md.contains("color: red"));
    }

    #[tokio::test]
    async fn test_web_fetch_tool_ssrf_blocking() {
        let strict_validator = EndpointValidator::new(false);

        // Cloud metadata address must be blocked
        let res_meta = WebFetchTool::fetch_url(
            "http://169.254.169.254/latest/meta-data/",
            &strict_validator,
        )
        .await;
        assert!(res_meta.is_err());
        assert!(res_meta.unwrap_err().contains("SSRF Protection"));

        // Loopback must be blocked when allow_localhost is false
        let res_local =
            WebFetchTool::fetch_url("http://127.0.0.1:8080/admin", &strict_validator).await;
        assert!(res_local.is_err());
        assert!(res_local.unwrap_err().contains("SSRF Protection"));

        // Invalid scheme must be blocked
        let res_ftp = WebFetchTool::fetch_url("ftp://example.com/file", &strict_validator).await;
        assert!(res_ftp.is_err());
        assert!(res_ftp.unwrap_err().contains("Unsupported scheme"));
    }

    struct MockSearchProvider {
        mock_results: Vec<SearchResult>,
    }

    #[async_trait]
    impl SearchProvider for MockSearchProvider {
        async fn search(&self, _query: &str) -> Result<Vec<SearchResult>, String> {
            Ok(self.mock_results.clone())
        }
    }

    #[tokio::test]
    async fn test_web_search_domain_filtering() {
        let mock_provider = MockSearchProvider {
            mock_results: vec![
                SearchResult {
                    title: "Rust Documentation".into(),
                    url: "https://docs.rs/tokio".into(),
                    snippet: "Tokio async runtime".into(),
                },
                SearchResult {
                    title: "GitHub Repo".into(),
                    url: "https://github.com/tokio-rs/tokio".into(),
                    snippet: "Tokio source repository".into(),
                },
                SearchResult {
                    title: "Blog Post".into(),
                    url: "https://bad-spam.com/article".into(),
                    snippet: "Spam content".into(),
                },
            ],
        };

        set_search_provider_override(Some(Arc::new(mock_provider)));
        let search_tool = WebSearchTool;
        let dummy_root = Path::new(".");

        // 1. Allowed domains only
        let res_allowed = search_tool
            .execute(
                json!({
                    "query": "tokio",
                    "allowed_domains": ["docs.rs", "github.com"]
                }),
                dummy_root,
            )
            .await
            .unwrap();

        assert!(res_allowed.contains("https://docs.rs/tokio"));
        assert!(res_allowed.contains("https://github.com/tokio-rs/tokio"));
        assert!(!res_allowed.contains("bad-spam.com"));

        // 2. Blocked domains
        let res_blocked = search_tool
            .execute(
                json!({
                    "query": "tokio",
                    "blocked_domains": ["bad-spam.com"]
                }),
                dummy_root,
            )
            .await
            .unwrap();

        assert!(res_blocked.contains("https://docs.rs/tokio"));
        assert!(!res_blocked.contains("bad-spam.com"));

        // Clean up override
        set_search_provider_override(None);
    }
}
