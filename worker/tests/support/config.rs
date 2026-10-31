#[derive(Debug, PartialEq, Eq)]
pub enum Config {
    Disabled,
    Enabled { repo: String },
}

impl Config {
    pub fn parse(cluster: Option<&str>, repo: Option<&str>) -> Result<Self, String> {
        if cluster != Some("1") {
            return Ok(Self::Disabled);
        }
        let repo = repo
            .filter(|repo| !repo.trim().is_empty())
            .ok_or("FS_TEST_IMAGE_REPO is required when FS_TEST_CLUSTER=1")?;
        if repo.starts_with('-')
            || repo.contains('@')
            || repo.chars().any(char::is_whitespace)
            || repo
                .rsplit('/')
                .next()
                .is_none_or(|last| last.is_empty() || last.contains(':'))
        {
            return Err("FS_TEST_IMAGE_REPO must be an untagged image repository".into());
        }
        Ok(Self::Enabled { repo: repo.into() })
    }
}
