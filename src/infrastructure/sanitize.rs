//! Output sanitization for anything derived from upstream responses.
//!
//! Upstream error bodies occasionally contain live-looking signed URLs,
//! temporary credentials or API keys. Nothing that reaches the MCP client may
//! echo those, so every failure message passes through [`redact`] on its way
//! out (see `ToolError::into_error_data`). Raw bodies stay in `tracing::debug!`
//! logs only, which are under the operator's control.

use std::sync::OnceLock;

use regex::Regex;

fn url_credentials() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(https?://)[^\s/@:]+:[^\s/@]+@").expect("valid regex")
    })
}

fn url_query() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(https?://[^\s"'<>)\]]*\?)\S*"#).expect("valid regex")
    })
}

fn secret_kv() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(access[_-]?key(?:[_-]?id)?|secret[_-]?access[_-]?key|signature|expires|token|api[_-]?key|secret|password|passwd|session[_-]?token)([=%:])([^\s&"',;)\]]+)"#,
        )
        .expect("valid regex")
    })
}

fn bearer_token() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer)(\s*=?\s*:?\s*)([A-Za-z0-9._\-]{8,})").expect("valid regex")
    })
}

fn aws_key_id() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b((?:AKIA|ABIA|ACCA|ASIA|LTAI)[0-9A-Za-z]{8,})\b").expect("valid regex")
    })
}

/// Redact credentials, secret-looking key/value pairs and URL query strings
/// from a string before it can be returned to the model.
pub fn redact(text: &str) -> String {
    let step = secret_kv().replace_all(text, "${1}${2}[redacted]");
    let step = bearer_token().replace_all(&step, "${1}${2}[redacted]");
    let step = aws_key_id().replace_all(&step, "[redacted-key-id]");
    let step = url_credentials().replace_all(&step, "${1}[redacted]@");
    url_query().replace_all(&step, "${1}[redacted]").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_signed_oss_style_urls() {
        let leaked = "upstream said: see https://bucket.oss-cn.aliyuncs.com/proxy_temp_file/x?OSSAccessKeyId=LTAI5tKoG9A3DkwGD635QVZr&Signature=abc%2Bdef&Expires=1817085764 done";
        let clean = redact(leaked);
        assert!(!clean.contains("LTAI5tKoG9A3DkwGD635QVZr"), "{clean}");
        assert!(!clean.contains("abc%2Bdef"), "{clean}");
        assert!(!clean.contains("1817085764"), "{clean}");
        assert!(clean.contains("?[redacted]"), "{clean}");
    }

    #[test]
    fn redacts_bearer_and_kv_secrets() {
        assert!(!redact("Authorization: Bearer sk-abc123def456").contains("sk-abc123def456"));
        assert!(!redact("invalid api_key=hunter2 provided").contains("hunter2"));
        assert!(!redact("Signature=zzzz1111&x").contains("zzzz1111"));
    }

    #[test]
    fn redacts_url_credentials_and_keeps_plain_text() {
        let clean = redact("connect via https://user:pw@db.corp/x failed");
        assert!(!clean.contains("pw@"), "{clean}");
        assert_eq!(redact("plain error message"), "plain error message");
    }

    #[test]
    fn keeps_clean_urls_intact() {
        let text = "see https://docs.rs/axum for details";
        assert_eq!(redact(text), text);
    }
}
