//! Who is asking: the contact details a few services want in a User-Agent.
//!
//! arXiv asks automated readers for a way to be reached, and Reddit's API
//! rules ask for the account behind a client. Both come from the environment
//! (`.env`), never from the code, so the repository names nobody. Unset, the
//! requests still go out, just without a contact.

const RESEARCH_AGENT: &str = "Pulse/1.0 (personal research reader)";
const REDDIT_AGENT: &str = "macos:com.pulse.fetcher:0.1.0";

/// The User-Agent for the research reader (arXiv). `PULSE_CONTACT_EMAIL`
/// adds a `mailto:`.
pub fn research_user_agent() -> String {
    research_agent(std::env::var("PULSE_CONTACT_EMAIL").ok().as_deref())
}

/// The User-Agent for Reddit's public JSON API. `PULSE_REDDIT_USER` adds the
/// `(by /u/<name>)` its rules ask for.
pub fn reddit_user_agent() -> String {
    reddit_agent(std::env::var("PULSE_REDDIT_USER").ok().as_deref())
}

fn research_agent(email: Option<&str>) -> String {
    match email.map(str::trim).filter(|e| is_email(e)) {
        Some(email) => format!("Pulse/1.0 (personal research reader; mailto:{email})"),
        None => RESEARCH_AGENT.to_string(),
    }
}

fn reddit_agent(user: Option<&str>) -> String {
    let name = user.map(|u| u.trim().trim_start_matches('/').trim_start_matches("u/"));
    match name.filter(|n| is_reddit_name(n)) {
        Some(name) => format!("{REDDIT_AGENT} (by /u/{name})"),
        None => REDDIT_AGENT.to_string(),
    }
}

/// Loose on purpose: enough to keep a typo or a stray character out of a
/// header. A value a header cannot carry would fail the whole client.
fn is_email(s: &str) -> bool {
    let Some((name, host)) = s.split_once('@') else {
        return false;
    };
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
    };
    s.len() <= 254 && plain(name) && plain(host) && host.contains('.')
}

/// Reddit names are 3 to 20 letters, digits, `_` or `-`.
fn is_reddit_name(s: &str) -> bool {
    (3..=20).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_contact_the_agents_name_nobody() {
        assert_eq!(research_agent(None), "Pulse/1.0 (personal research reader)");
        assert_eq!(reddit_agent(None), "macos:com.pulse.fetcher:0.1.0");
        assert_eq!(research_agent(Some("  ")), RESEARCH_AGENT);
        assert_eq!(reddit_agent(Some("")), REDDIT_AGENT);
    }

    #[test]
    fn a_contact_from_the_environment_is_added() {
        assert_eq!(
            research_agent(Some(" reader@example.com ")),
            "Pulse/1.0 (personal research reader; mailto:reader@example.com)"
        );
        for typed in [
            "some_reader",
            "u/some_reader",
            "/u/some_reader",
            " some_reader ",
        ] {
            assert_eq!(
                reddit_agent(Some(typed)),
                "macos:com.pulse.fetcher:0.1.0 (by /u/some_reader)",
                "{typed}"
            );
        }
    }

    #[test]
    fn a_value_a_header_cannot_carry_is_left_out() {
        for bad in [
            "not an email",
            "a@b",
            "a b@example.com",
            "a@exa mple.com",
            "x@example.com\nX-Evil: 1",
            "é@example.com",
        ] {
            assert_eq!(research_agent(Some(bad)), RESEARCH_AGENT, "{bad}");
        }
        for bad in [
            "ab",
            "has space",
            "name\nX-Evil: 1",
            "a/b/c",
            "nomé",
            "a_name_far_too_long_for_reddit",
        ] {
            assert_eq!(reddit_agent(Some(bad)), REDDIT_AGENT, "{bad}");
        }
    }

    #[test]
    fn every_agent_is_a_valid_header_value() {
        for agent in [
            research_agent(None),
            research_agent(Some("reader+pulse@example.co.uk")),
            reddit_agent(None),
            reddit_agent(Some("some-reader_1")),
        ] {
            assert!(
                reqwest::header::HeaderValue::from_str(&agent).is_ok(),
                "{agent}"
            );
        }
    }

    /// The point of this file: no address or account name comes back into the
    /// sources that used to carry one.
    #[test]
    fn the_code_names_nobody() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for file in [
            "research/mod.rs",
            "research/fulltext.rs",
            "sources/reddit.rs",
        ] {
            let text = std::fs::read_to_string(src.join(file)).unwrap_or_default();
            assert!(!text.is_empty(), "{file} is missing");
            assert!(!text.contains("mailto:"), "{file} holds an address");
            assert!(!text.contains("/u/"), "{file} holds an account name");
        }
    }
}
