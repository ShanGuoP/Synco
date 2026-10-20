//! 双向交接的输入声明；不读取外部文件或数据库。
use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub path: String,
    pub sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub format: String,
    #[serde(default = "quality")]
    pub quality: u8,
}
fn quality() -> u8 {
    95
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub task_id: String,
    pub client_id: String,
    pub source_project_id: String,
    pub project_name: String,
    pub source_image_id: String,
    pub input: Input,
    pub output: Output,
    #[serde(default)]
    pub open_in_editor: bool,
}
impl Create {
    pub fn validate(&self) -> Result<()> {
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            if !matches!(std::path::Path::new(&self.input.path).components().next(),Some(Component::Prefix(p)) if matches!(p.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_)))
            {
                return Err(AppError::bad("srv.interop.bad_request"));
            }
        }
        let id = &self.task_id;
        let uuid = id.len() == 36
            && id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            });
        let valid = uuid
            && client_valid(&self.client_id)
            && [&self.source_project_id, &self.source_image_id]
                .iter()
                .all(|v| !v.trim().is_empty() && v.len() <= 256)
            && !self.project_name.trim().is_empty()
            && self.project_name.chars().count() <= 60
            && self.input.path.len() <= 32768
            && std::path::Path::new(&self.input.path).is_absolute()
            && self.input.sha256.len() == 64
            && self
                .input
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && (1..=100).contains(&self.output.quality);
        if !valid {
            return Err(AppError::bad("srv.interop.bad_request"));
        }
        if !["jpeg", "png"].contains(&self.output.format.as_str()) {
            return Err(AppError::coded(415, "srv.interop.unsupported_format"));
        }
        Ok(())
    }
}
pub fn client_valid(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 80
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn uuid() -> String {
    let mut b = rand::random::<[u8; 16]>();
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    let s: String = b.iter().map(|n| format!("{n:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    )
}
