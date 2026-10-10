//! What looks like a secret, so that it is never kept: a value under a
//! name that says it is one, or a value that looks like one whatever its
//! name (a private key, a token, a password in a URL).

use std::sync::LazyLock;

use regex::Regex;

/// What a hidden value is shown as.
pub(crate) const HIDDEN: &str = "<hidden>";

/// Whether a setting's name says it holds a secret: one of its words is a
/// password, a token, a secret, a credential or a private key. A key's
/// *name* or *id* (a key pair's name, a KMS key's ARN) is not one.
pub(crate) fn secret_name(name: &str) -> bool {
    let words = words(name);
    let has = |w: &str| words.iter().any(|x| x == w);
    const NOT_ITSELF: [&str; 16] = [
        "ref", "arn", "name", "id", "file", "path", "url", "uri", "endpoint", "host", "type",
        "ttl", "expiry", "length", "header", "mount",
    ];
    if NOT_ITSELF.iter().any(|w| has(w)) {
        return false;
    }
    has("password")
        || has("passwd")
        || has("pwd")
        || has("secret")
        || has("token")
        || has("credential")
        || has("credentials")
        || (has("key") && (has("api") || has("private") || has("access") || has("secret")))
        || has("apikey")
        || has("privatekey")
}

/// The lowercase words of a name, split on case and punctuation.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a value looks like a secret whatever its name.
pub(crate) fn secret_value(value: &str) -> bool {
    static SHAPES: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(concat!(
            r"-----BEGIN [A-Z ]*PRIVATE KEY|",
            // A JSON web token.
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.|",
            // Cloud access keys and well-known token prefixes.
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b|",
            r"\b(?:ghp|gho|ghs|ghu|github_pat|glpat|xox[abp]|sk-[a-z]*)[_-][A-Za-z0-9_-]{16,}",
        ))
        .ok()
    });
    SHAPES.as_ref().is_some_and(|re| re.is_match(value)) || url_password(value).is_some()
}

/// The password in a URL's user info, `scheme://user:password@host`.
fn url_password(value: &str) -> Option<(usize, usize)> {
    static USERINFO: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"[a-z][a-z0-9+.-]*://[^\s:/@]+:([^\s/@]+)@").ok());
    let re = USERINFO.as_ref()?;
    let found = re.captures(value)?.get(1)?;
    // `${VAR}` and `$(VAR)` are where a secret goes, not a secret.
    let text = found.as_str();
    if text.starts_with('$') || text.starts_with('{') || text.starts_with('<') {
        return None;
    }
    Some((found.start(), found.end()))
}

/// `value` as it may be kept for the setting `name`: hidden whole when the
/// name says it is a secret or the value looks like one, a URL's password
/// taken out.
pub(crate) fn keep(name: &str, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if let Some((start, end)) = url_password(value) {
        let rest = format!("{}{HIDDEN}{}", &value[..start], &value[end..]);
        return if secret_value(&rest) {
            HIDDEN.to_owned()
        } else {
            rest
        };
    }
    if secret_name(name) && !references(value) || secret_value(value) {
        HIDDEN.to_owned()
    } else {
        value.to_owned()
    }
}

/// Whether a value only names where a secret is, rather than holding it:
/// a variable, a template, a path, a secret store's ARN.
fn references(value: &str) -> bool {
    value.starts_with('$')
        || value.starts_with("{{")
        || value.starts_with("arn:")
        || value.starts_with('/')
        || value.contains("${")
}

/// A line of code or settings as it may be kept as evidence: the value of
/// a `name = value` or `name: value` whose name says it is a secret
/// hidden, and any secret-looking value too.
pub(crate) fn scrub_line(line: &str) -> String {
    static ASSIGNMENT: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r#"^(\s*-?\s*["']?([A-Za-z_][\w.-]*)["']?\s*[:=]\s*)(.+)$"#).ok()
    });
    if let Some(captures) = ASSIGNMENT.as_ref().and_then(|re| re.captures(line))
        && secret_name(&captures[2])
    {
        let value = captures[3].trim().trim_matches(['"', '\'', ',']);
        if !references(value) {
            return format!("{}{HIDDEN}", &captures[1]);
        }
    }
    if let Some((start, end)) = url_password(line) {
        let rest = format!("{}{HIDDEN}{}", &line[..start], &line[end..]);
        return scrub_line(&rest);
    }
    if secret_value(line) {
        return HIDDEN.to_owned();
    }
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_say_secret() {
        for name in [
            "DB_PASSWORD",
            "apiKey",
            "client_secret",
            "GITHUB_TOKEN",
            "privateKey",
            "aws.secretAccessKey",
        ] {
            assert!(secret_name(name), "{name}");
        }
        for name in [
            "secretName",
            "tokenUrl",
            "key_name",
            "kms_key_arn",
            "secretRef",
            "keys",
            "passwordFile",
            "image.tag",
        ] {
            assert!(!secret_name(name), "{name}");
        }
    }

    #[test]
    fn values_kept_or_hidden() {
        assert_eq!(keep("DB_PASSWORD", "hunter22"), HIDDEN);
        assert_eq!(keep("DB_PASSWORD", "${DB_PASSWORD}"), "${DB_PASSWORD}");
        assert_eq!(keep("replicas", "3"), "3");
        assert_eq!(
            keep("DATABASE_URL", "postgresql://app:s3cret@db:5432/app"),
            "postgresql://app:<hidden>@db:5432/app"
        );
        assert_eq!(keep("anything", "AKIAABCDEFGHIJKLMNOP"), HIDDEN);
        assert_eq!(
            scrub_line("  API_KEY: \"abc123def456\""),
            "  API_KEY: <hidden>"
        );
        assert_eq!(
            scrub_line("url = \"http://api:8000/v1\""),
            "url = \"http://api:8000/v1\""
        );
        assert_eq!(
            scrub_line("DB=postgres://u:pw@db/x"),
            "DB=postgres://u:<hidden>@db/x"
        );
    }
}
