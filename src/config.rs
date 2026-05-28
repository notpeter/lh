use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::common::LhResult;
use crate::util::{APP_DIR_NAME, canonicalize_existing, home_dir};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
    pub llm: Option<LlmConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    pub prompt: String,
}

pub fn config_path() -> PathBuf {
    config_path_for(&home_dir())
}

pub fn config_path_for(home: &Path) -> PathBuf {
    home.join(".config").join(format!("{APP_DIR_NAME}.toml"))
}

pub fn load() -> LhResult<Config> {
    let path = config_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(error.into()),
    };
    Ok(toml::from_str(&text)?)
}

pub fn save(config: &Config) -> LhResult<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, toml::to_string_pretty(config)?)?;
    Ok(path)
}

pub fn add_alias(base: &Path, source: &Path, target: &Path) -> LhResult<(String, String, PathBuf)> {
    let source = normalize_dir(base, source)?;
    let target = normalize_dir(base, target)?;
    if source == target {
        return Err("alias source and target are the same directory".into());
    }

    let mut config = load()?;
    let source = compact_home_path(&source);
    let target = compact_home_path(&target);
    config.alias.insert(source.clone(), target.clone());
    let path = save(&config)?;
    Ok((source, target, path))
}

pub fn remove_alias(base: &Path, dir: &Path) -> LhResult<(Vec<(String, String)>, PathBuf)> {
    let dir = normalize_dir(base, dir)?;
    let mut config = load()?;
    let removed = remove_alias_from_config(&mut config, &dir);
    let path = save(&config)?;
    Ok((removed, path))
}

pub fn alias_group(cwd: &Path) -> LhResult<Vec<PathBuf>> {
    let config = load()?;
    Ok(alias_group_from_config(&config, cwd))
}

pub fn all_alias_dirs() -> LhResult<Vec<PathBuf>> {
    let config = load()?;
    Ok(all_alias_dirs_from_config(&config))
}

pub fn aliases_for_dir(cwd: &Path) -> LhResult<Vec<(String, String)>> {
    let config = load()?;
    Ok(aliases_for_dir_from_config(&config, cwd))
}

pub fn normalize_dir(base: &Path, path: &Path) -> LhResult<PathBuf> {
    let expanded = expand_home_path(path);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    let canonical = canonicalize_existing(&absolute);
    if !canonical.is_dir() {
        return Err(format!("not a directory: {}", canonical.display()).into());
    }
    Ok(canonical)
}

pub fn compact_home_path(path: &Path) -> String {
    let home = home_dir();
    if path == home {
        return "~".to_string();
    }
    if let Ok(stripped) = path.strip_prefix(&home) {
        return format!("~/{}", stripped.display());
    }
    path.display().to_string()
}

fn alias_group_from_config(config: &Config, cwd: &Path) -> Vec<PathBuf> {
    let start = canonicalize_existing(cwd);
    let edges = alias_edges(config);
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([start.clone()]);
    let mut dirs = Vec::new();

    while let Some(dir) = queue.pop_front() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        dirs.push(dir.clone());
        for (left, right) in &edges {
            if *left == dir {
                queue.push_back(right.clone());
            } else if *right == dir {
                queue.push_back(left.clone());
            }
        }
    }

    dirs
}

fn all_alias_dirs_from_config(config: &Config) -> Vec<PathBuf> {
    let mut dirs = BTreeSet::new();
    for (source, target) in alias_edges(config) {
        dirs.insert(source);
        dirs.insert(target);
    }
    dirs.into_iter().collect()
}

fn aliases_for_dir_from_config(config: &Config, cwd: &Path) -> Vec<(String, String)> {
    let group = alias_group_from_config(config, cwd)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if group.len() <= 1 {
        return Vec::new();
    }

    config
        .alias
        .iter()
        .filter_map(|(source, target)| {
            let source_path = canonicalize_existing(&expand_home(source));
            let target_path = canonicalize_existing(&expand_home(target));
            (group.contains(&source_path) || group.contains(&target_path))
                .then(|| (source.clone(), target.clone()))
        })
        .collect()
}

fn remove_alias_from_config(config: &mut Config, dir: &Path) -> Vec<(String, String)> {
    let mut removed = Vec::new();
    let mut remaining = BTreeMap::new();

    for (source, target) in std::mem::take(&mut config.alias) {
        let source_path = canonicalize_existing(&expand_home(&source));
        let target_path = canonicalize_existing(&expand_home(&target));
        if source_path == dir || target_path == dir {
            removed.push((source, target));
        } else {
            remaining.insert(source, target);
        }
    }

    config.alias = remaining;
    removed
}

fn alias_edges(config: &Config) -> Vec<(PathBuf, PathBuf)> {
    config
        .alias
        .iter()
        .map(|(source, target)| {
            (
                canonicalize_existing(&expand_home(source)),
                canonicalize_existing(&expand_home(target)),
            )
        })
        .collect()
}

fn expand_home_path(path: &Path) -> PathBuf {
    let Some(path) = path.to_str() else {
        return path.to_path_buf();
    };
    expand_home(path)
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::temp_dir;

    #[test]
    fn chooses_config_path() {
        assert_eq!(
            config_path_for(Path::new("/home/me")),
            PathBuf::from("/home/me/.config/llm-history.toml")
        );
    }

    #[test]
    fn alias_groups_are_transitive() {
        let root = temp_dir("alias-group");
        let one = root.join("one");
        let two = root.join("two");
        let three = root.join("three");
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        fs::create_dir_all(&three).unwrap();

        let config = Config {
            alias: BTreeMap::from([
                (one.display().to_string(), two.display().to_string()),
                (three.display().to_string(), two.display().to_string()),
            ]),
            ..Default::default()
        };

        let one = canonicalize_existing(&one);
        let two = canonicalize_existing(&two);
        let three = canonicalize_existing(&three);
        let group = alias_group_from_config(&config, &one);
        assert!(group.contains(&one));
        assert!(group.contains(&two));
        assert!(group.contains(&three));
    }

    #[test]
    fn aliases_for_dir_only_returns_connected_edges() {
        let root = temp_dir("aliases-for-dir");
        let one = root.join("one");
        let two = root.join("two");
        let three = root.join("three");
        let unrelated = root.join("unrelated");
        let other = root.join("other");
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        fs::create_dir_all(&three).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::create_dir_all(&other).unwrap();

        let config = Config {
            alias: BTreeMap::from([
                (one.display().to_string(), two.display().to_string()),
                (three.display().to_string(), two.display().to_string()),
                (unrelated.display().to_string(), other.display().to_string()),
            ]),
            ..Default::default()
        };

        let aliases = aliases_for_dir_from_config(&config, &one);

        assert_eq!(
            aliases,
            vec![
                (one.display().to_string(), two.display().to_string()),
                (three.display().to_string(), two.display().to_string()),
            ]
        );
    }

    #[test]
    fn aliases_for_dir_returns_empty_for_unaliased_dir() {
        let root = temp_dir("aliases-for-unaliased-dir");
        let one = root.join("one");
        let two = root.join("two");
        let unrelated = root.join("unrelated");
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        fs::create_dir_all(&unrelated).unwrap();

        let config = Config {
            alias: BTreeMap::from([(one.display().to_string(), two.display().to_string())]),
            ..Default::default()
        };

        assert!(aliases_for_dir_from_config(&config, &unrelated).is_empty());
    }

    #[test]
    fn removes_alias_edges_for_dir_on_either_side() {
        let root = temp_dir("remove-alias");
        let one = root.join("one");
        let two = root.join("two");
        let three = root.join("three");
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        fs::create_dir_all(&three).unwrap();

        let mut config = Config {
            alias: BTreeMap::from([
                (one.display().to_string(), two.display().to_string()),
                (three.display().to_string(), two.display().to_string()),
            ]),
            ..Default::default()
        };
        let two = canonicalize_existing(&two);

        let removed = remove_alias_from_config(&mut config, &two);

        assert_eq!(removed.len(), 2);
        assert!(config.alias.is_empty());
    }
}
