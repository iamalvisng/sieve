//! The product name set. Every product-facing name derives from it.

/// One product name set. Every product-facing name derives from `name`.
pub struct Product {
    /// The bare product name: the binary, the context dir, the prefix.
    pub name: &'static str,
    /// The command the update nudge prints.
    pub upgrade_command: &'static str,
}

impl Product {
    /// Returns the name of the context directory.
    pub fn context_dir_name(&self) -> &'static str {
        self.name
    }

    /// Returns the bracketed log prefix.
    pub fn prefix(&self) -> String {
        format!("[{}]", self.name)
    }

    /// Returns the tool name for a base tool suffix.
    pub fn tool(&self, base: &str) -> String {
        format!("{}_{}", self.name, base)
    }

    /// Returns the upper-case environment variable name for a suffix.
    pub fn env_var(&self, suffix: &str) -> String {
        format!("{}_{}", self.name.to_uppercase(), suffix)
    }

    /// Reads the environment variable for a suffix, if set.
    pub fn env(&self, suffix: &str) -> Option<String> {
        std::env::var(self.env_var(suffix)).ok()
    }

    /// Returns the name of the per-user home directory.
    pub fn home_dir_name(&self) -> String {
        format!(".{}", self.name)
    }

    /// Returns the file name of the Claude Code hooks script.
    pub fn hooks_file(&self) -> String {
        format!("{}-hooks.cjs", self.name)
    }

    /// Returns the file name of the statusline script.
    pub fn statusline_file(&self) -> String {
        format!("{}-statusline.cjs", self.name)
    }

    /// Returns the MCP server name.
    pub fn server_name(&self) -> &'static str {
        self.name
    }

    /// Returns the name of the skill directory.
    pub fn skill_dir(&self) -> &'static str {
        self.name
    }

    /// Returns the key this product uses in MCP config maps.
    pub fn mcp_key(&self) -> &'static str {
        self.name
    }
}

/// The Sieve name set.
pub const SIEVE: Product = Product {
    name: "sieve",
    upgrade_command: "cargo install sieve-cli",
};

/// Returns the one name set. Every name in the product derives from it.
pub fn product() -> &'static Product {
    &SIEVE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_dir_name_is_bare_name() {
        assert_eq!(SIEVE.context_dir_name(), "sieve");
    }

    #[test]
    fn prefix_wraps_name_in_brackets() {
        assert_eq!(SIEVE.prefix(), "[sieve]");
    }

    #[test]
    fn tool_joins_name_and_base() {
        assert_eq!(SIEVE.tool("find_code"), "sieve_find_code");
    }

    #[test]
    fn env_var_upper_cases_name() {
        assert_eq!(SIEVE.env_var("NO_REFRESH"), "SIEVE_NO_REFRESH");
    }

    #[test]
    fn home_dir_name_adds_dot() {
        assert_eq!(SIEVE.home_dir_name(), ".sieve");
    }

    #[test]
    fn hooks_file_names_the_script() {
        assert_eq!(SIEVE.hooks_file(), "sieve-hooks.cjs");
    }

    #[test]
    fn statusline_file_names_the_script() {
        assert_eq!(SIEVE.statusline_file(), "sieve-statusline.cjs");
    }

    #[test]
    fn server_name_is_bare_name() {
        assert_eq!(SIEVE.server_name(), "sieve");
    }

    #[test]
    fn skill_dir_is_bare_name() {
        assert_eq!(SIEVE.skill_dir(), "sieve");
    }

    #[test]
    fn mcp_key_is_bare_name() {
        assert_eq!(SIEVE.mcp_key(), "sieve");
    }
}
