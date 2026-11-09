//! An agent thirdeye detects but cannot serve.
//!
//! Named so a user sees thirdeye noticed their agent, and told the truth
//! about it, rather than silently ignoring it.

use anyhow::{Result, bail};

use super::{Context, Inspection, Integration, Scope, Target};

pub struct Unsupported {
    pub id: &'static str,
    pub program: &'static str,
}

impl Integration for Unsupported {
    fn id(&self) -> &'static str {
        self.id
    }

    fn program(&self) -> &'static str {
        self.program
    }

    fn scopes(&self) -> &'static [Scope] {
        &[]
    }

    fn publish(&self, _: &Context) -> Result<()> {
        Ok(())
    }

    // Silently doing nothing would read as success and leave the user
    // waiting for an integration that was never attempted.
    fn install(&self, _: &Context, _: &Target) -> Result<()> {
        bail!(
            "{} is not supported: {}",
            self.id,
            self.unsupported_reason()
        )
    }

    fn uninstall(&self, _: &Context, _: &Target) -> Result<()> {
        bail!(
            "{} is not supported: {}",
            self.id,
            self.unsupported_reason()
        )
    }

    fn inspect(&self, _: &Context, _: &Target) -> Option<Inspection> {
        None
    }

    fn next_steps(&self, _: &Target) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{context, no_vars, scratch};
    use super::*;

    #[test]
    fn an_unsupported_agent_refuses_rather_than_pretending() {
        let base = scratch("unsupported");
        let run = |_: &str, _: &[&str], _: Option<&std::path::Path>| Ok(String::new());
        let cx = context(&base, &no_vars, &run);
        let gemini = super::super::by_id("gemini").unwrap();
        assert_eq!(gemini.program(), "gemini");
        let err = gemini.install(&cx, &Target::User).unwrap_err().to_string();
        assert!(
            err.contains("gemini") && err.contains("no adapter"),
            "{err}"
        );
    }
}
