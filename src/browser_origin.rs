//! Explicit root-hosted TLS proxy origin; forwarded headers are never trusted.
#[derive(Clone, Debug)]
pub struct ExternalOrigin(String);

impl ExternalOrigin {
    pub fn parse(value: &str) -> Result<Self, String> {
        let invalid = || {
            "AGENTWORKS_EXTERNAL_ORIGIN must be an HTTPS origin without a path, credentials, query or fragment".to_string()
        };
        let uri: axum::http::Uri = value.parse().map_err(|_| invalid())?;
        let authority = uri.authority().ok_or_else(invalid)?;
        if uri.scheme_str() != Some("https")
            || authority.as_str().contains('@')
            || value.contains('#')
            || !matches!(uri.path(), "" | "/")
            || uri.query().is_some()
        {
            return Err(invalid());
        }
        let authority = authority.as_str().to_ascii_lowercase();
        let authority = authority.strip_suffix(":443").unwrap_or(&authority);
        Ok(Self(format!("https://{authority}")))
    }

    pub fn allows_host(&self, host: &str) -> bool {
        host == &self.0[8..]
    }

    pub fn allows_origin(&self, origin: &str) -> bool {
        origin == self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_configured_https_origin_and_authority_are_allowed() {
        let external = ExternalOrigin::parse("https://AgentBox.example:443/").unwrap();
        assert!(external.allows_host("agentbox.example"));
        assert!(external.allows_origin("https://agentbox.example"));
        for origin in [
            "http://agentbox.example",
            "https://other.example",
            "https://agentbox.example.evil",
            "null",
            "",
        ] {
            assert!(!external.allows_origin(origin));
        }
        assert!(!external.allows_host("other.example"));
        let external = ExternalOrigin::parse("https://localhost:4414").unwrap();
        assert!(external.allows_host("localhost:4414"));
        assert!(!external.allows_host("localhost"));
    }

    #[test]
    fn reject_non_origin_configuration() {
        for value in [
            "http://localhost",
            "https://user@host",
            "https://host/path",
            "https://host?query",
            "https://host/#fragment",
            "*",
            "null",
            "",
        ] {
            assert!(ExternalOrigin::parse(value).is_err(), "{value}");
        }
    }
}
