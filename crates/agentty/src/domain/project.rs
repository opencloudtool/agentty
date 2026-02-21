use std::path::PathBuf;

pub struct Project {
    pub git_branch: Option<String>,
    pub id: i64,
    pub path: PathBuf,
}
