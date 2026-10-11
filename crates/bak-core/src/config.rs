//! `.bak/config.toml`: the tracked paths and named remotes of one project.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const BAK_DIR: &str = ".bak";
pub const CONFIG_FILE: &str = "config.toml";
pub const IGNORE_FILE: &str = ".bakignore";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub project: ProjectSection,
    #[serde(default)]
    pub remotes: BTreeMap<String, RemoteConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    pub name: String,
    #[serde(default)]
    pub tracked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_mount: bool,
    /// CA certificate for a REST server with a self-signed certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cacert: Option<String>,
    /// Target size of data pack files, in MiB. Only used when the repository is created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_size_mib: Option<u32>,
    /// File holding `username:password` for a REST remote. Must have mode 0600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_file: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A project: its root directory and parsed config.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: Config,
}

impl Project {
    /// Create `.bak/config.toml` in `dir`. Tracks the whole project by default.
    pub fn init(dir: &Path) -> Result<Self> {
        let root = dir
            .canonicalize()
            .map_err(|e| Error::io(format!("cannot open {}", dir.display()), e))?;
        let config_path = root.join(BAK_DIR).join(CONFIG_FILE);
        if config_path.exists() {
            return Err(Error::ProjectExists(root));
        }
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string();
        let project = Self {
            root,
            config: Config {
                project: ProjectSection {
                    name,
                    tracked: vec![".".to_string()],
                },
                remotes: BTreeMap::new(),
            },
        };
        project.save()?;
        Ok(project)
    }

    /// Find the project containing `start` by walking up to the nearest `.bak/`.
    pub fn discover(start: &Path) -> Result<Self> {
        let start = start
            .canonicalize()
            .map_err(|e| Error::io(format!("cannot open {}", start.display()), e))?;
        let mut dir = Some(start.as_path());
        while let Some(d) = dir {
            let config_path = d.join(BAK_DIR).join(CONFIG_FILE);
            if config_path.is_file() {
                let text = fs::read_to_string(&config_path)
                    .map_err(|e| Error::io(format!("cannot read {}", config_path.display()), e))?;
                let config: Config =
                    toml::from_str(&text).map_err(|e| Error::Config(e.to_string()))?;
                return Ok(Self {
                    root: d.to_path_buf(),
                    config,
                });
            }
            dir = d.parent();
        }
        Err(Error::NoProject(start))
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join(BAK_DIR).join(CONFIG_FILE)
    }

    pub fn save(&self) -> Result<()> {
        let dir = self.root.join(BAK_DIR);
        fs::create_dir_all(&dir)
            .map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
        let text =
            toml::to_string_pretty(&self.config).map_err(|e| Error::Config(e.to_string()))?;
        let path = self.config_path();
        // Write then rename so an interrupted save never leaves a half-written config.
        let tmp = dir.join("config.toml.tmp");
        fs::write(&tmp, text)
            .map_err(|e| Error::io(format!("cannot write {}", tmp.display()), e))?;
        fs::rename(&tmp, &path)
            .map_err(|e| Error::io(format!("cannot write {}", path.display()), e))
    }

    /// Turn a user-supplied path into one relative to the project root, using
    /// `/` separators. `"."` means the root itself.
    pub fn relative_path(&self, cwd: &Path, path: &Path, must_exist: bool) -> Result<String> {
        let abs = normalize(&cwd.join(path));
        let abs = if must_exist {
            if !abs.exists() && abs.symlink_metadata().is_err() {
                return Err(Error::MissingPath(abs));
            }
            // Canonicalise the parent only, so a tracked symlink stays a symlink.
            match (abs.parent(), abs.file_name()) {
                (Some(parent), Some(name)) => parent
                    .canonicalize()
                    .map_err(|e| Error::io(format!("cannot open {}", parent.display()), e))?
                    .join(name),
                _ => abs,
            }
        } else {
            abs
        };
        let rel = abs
            .strip_prefix(&self.root)
            .map_err(|_| Error::OutsideProject(abs.clone()))?;
        if rel.as_os_str().is_empty() {
            return Ok(".".to_string());
        }
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        parts
            .map(|p| p.join("/"))
            .ok_or_else(|| Error::NonUtf8Path(abs.clone()))
    }

    /// Add paths to the tracked list. Returns the paths that were newly added.
    pub fn track(&mut self, paths: Vec<String>) -> Vec<String> {
        let mut added = Vec::new();
        for p in paths {
            if !self.config.project.tracked.contains(&p) {
                self.config.project.tracked.push(p.clone());
                added.push(p);
            }
        }
        self.config.project.tracked.sort();
        added
    }

    /// Remove paths from the tracked list. Returns the paths that were removed.
    pub fn untrack(&mut self, paths: &[String]) -> Vec<String> {
        let mut removed = Vec::new();
        self.config.project.tracked.retain(|t| {
            let hit = paths.contains(t);
            if hit {
                removed.push(t.clone());
            }
            !hit
        });
        removed
    }

    pub fn remote(&self, name: &str) -> Result<&RemoteConfig> {
        self.config
            .remotes
            .get(name)
            .ok_or_else(|| Error::UnknownRemote(name.to_string()))
    }
}

/// Resolve `.` and `..` lexically, without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let text = r#"
[project]
name = "myproj"
tracked = ["."]

[remotes.usb]
url = "/media/user/USB1/bak/myproj"
require_mount = true

[remotes.home]
url = "rest:https://backup.example.internal:8000/myproj"
cacert = "~/.config/bak/home-ca.pem"
"#;
        let c: Config = toml::from_str(text).unwrap();
        assert!(c.remotes["usb"].require_mount);
        assert_eq!(c.project.tracked, vec!["."]);
    }

    #[test]
    fn normalize_resolves_dots() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }
}
