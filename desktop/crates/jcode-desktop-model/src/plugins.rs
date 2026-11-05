//! Claude Code and Codex plugins seen from Jcode.
//!
//! Lists what each tool has installed, reads the marketplaces each tool knows
//! about, and installs a plugin into Jcode: its skills go to
//! `~/.jcode/skills/` and its MCP servers to `~/.jcode/mcp.json`, the two
//! places the Jcode runtime already reads. Every install is recorded in
//! `~/.jcode/plugins/installed.json` so uninstall removes exactly what it
//! added and never a skill or server the user made by hand.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ecosystem {
    Claude,
    Codex,
    Jcode,
}

impl Ecosystem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Jcode => "Jcode",
        }
    }
}

/// Where the home folders live. Tests point these at temporary directories.
#[derive(Clone, Debug)]
pub struct Roots {
    pub home: PathBuf,
    pub jcode_home: PathBuf,
}

impl Roots {
    pub fn detect() -> Option<Self> {
        Some(Self {
            home: crate::platform_dirs::home_dir()?,
            jcode_home: crate::platform_dirs::jcode_home()?,
        })
    }

    fn claude_plugins(&self) -> PathBuf {
        self.home.join(".claude").join("plugins")
    }

    fn codex_home(&self) -> PathBuf {
        self.home.join(".codex")
    }

    fn plugins_dir(&self) -> PathBuf {
        self.jcode_home.join("plugins")
    }

    fn record_path(&self) -> PathBuf {
        self.plugins_dir().join("installed.json")
    }

    fn mcp_path(&self) -> PathBuf {
        self.jcode_home.join("mcp.json")
    }

    fn skills_dir(&self) -> PathBuf {
        self.jcode_home.join("skills")
    }
}

/// What a plugin directory provides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contents {
    pub name: Option<String>,
    /// `interface.displayName` from a Codex manifest.
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub commands: usize,
    pub agents: usize,
    /// Hosted connectors (Codex `.app.json`). They only work inside Codex.
    pub apps: usize,
}

/// A plugin installed in Claude Code, Codex or Jcode.
#[derive(Clone, Debug)]
pub struct InstalledPlugin {
    pub ecosystem: Ecosystem,
    pub marketplace: String,
    pub name: String,
    pub enabled: bool,
    pub path: PathBuf,
    pub contents: Contents,
    /// Also installed into Jcode (always true for `Ecosystem::Jcode`).
    pub in_jcode: bool,
}

impl InstalledPlugin {
    pub fn key(&self) -> String {
        format!("{}@{}", self.name, self.marketplace)
    }

    pub fn title(&self) -> &str {
        self.contents.display_name.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Local(PathBuf),
    Git {
        url: String,
        subdir: Option<String>,
        git_ref: Option<String>,
    },
}

/// A plugin a marketplace offers.
#[derive(Clone, Debug)]
pub struct MarketplacePlugin {
    pub ecosystem: Ecosystem,
    pub marketplace: String,
    pub name: String,
    pub description: String,
    pub category: Option<String>,
    pub source: Source,
    pub in_jcode: bool,
}

impl MarketplacePlugin {
    pub fn key(&self) -> String {
        format!("{}@{}", self.name, self.marketplace)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMode {
    /// Skills and MCP servers.
    Full,
    SkillsOnly,
}

/// One entry of `~/.jcode/plugins/installed.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub ecosystem: Ecosystem,
    pub marketplace: String,
    pub name: String,
    pub path: PathBuf,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
    /// Skills or servers left alone because the user already has one by
    /// that name.
    pub skipped: Vec<String>,
    /// Parts Jcode cannot use (Codex hosted apps, slash commands, agents).
    pub unsupported: Vec<String>,
}

// ---------------------------------------------------------------------------
// Reading plugin folders

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn manifest(dir: &Path) -> Option<Value> {
    [".claude-plugin/plugin.json", ".codex-plugin/plugin.json"]
        .iter()
        .find_map(|rel| read_json(&dir.join(rel)))
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

/// The manifest may name its skills folder or MCP file; otherwise the usual
/// place is used.
fn manifest_path(dir: &Path, manifest: Option<&Value>, key: &str, default: &str) -> PathBuf {
    let rel = manifest
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or(default);
    dir.join(rel.trim_start_matches("./"))
}

fn skill_dirs(dir: &Path, manifest: Option<&Value>) -> Vec<(String, PathBuf)> {
    let root = manifest_path(dir, manifest, "skills", "skills");
    let mut skills: Vec<_> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("SKILL.md").is_file())
        .filter_map(|path| Some((path.file_name()?.to_string_lossy().into_owned(), path)))
        .collect();
    skills.sort();
    skills
}

/// MCP servers from `.mcp.json`, in either `{"mcpServers": {..}}` or bare
/// `{name: config}` form, or inline in the manifest.
fn mcp_servers(dir: &Path, manifest: Option<&Value>) -> Map<String, Value> {
    if let Some(Value::Object(inline)) = manifest.and_then(|m| m.get("mcpServers")) {
        return inline.clone();
    }
    let file = manifest_path(dir, manifest, "mcpServers", ".mcp.json");
    match read_json(&file) {
        Some(Value::Object(mut map)) => match map.remove("mcpServers") {
            Some(Value::Object(servers)) => servers,
            _ => map.into_iter().filter(|(_, v)| v.is_object()).collect(),
        },
        _ => Map::new(),
    }
}

fn count_files(dir: &Path, extension: &str) -> usize {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == extension))
        .count()
}

pub fn scan(dir: &Path) -> Contents {
    let manifest = manifest(dir);
    let m = manifest.as_ref();
    let apps = read_json(&dir.join(".app.json"))
        .and_then(|v| v.get("apps").and_then(Value::as_object).map(Map::len))
        .unwrap_or(0);
    Contents {
        name: m.and_then(|m| str_field(m, "name")),
        display_name: m
            .and_then(|m| m.get("interface"))
            .and_then(|i| str_field(i, "displayName")),
        description: m.and_then(|m| str_field(m, "description")),
        version: m.and_then(|m| str_field(m, "version")),
        skills: skill_dirs(dir, m).into_iter().map(|(name, _)| name).collect(),
        mcp_servers: mcp_servers(dir, m).keys().cloned().collect(),
        commands: count_files(&dir.join("commands"), "md"),
        agents: count_files(&dir.join("agents"), "md"),
        apps,
    }
}

// ---------------------------------------------------------------------------
// Installed plugins

fn split_key(key: &str) -> (String, String) {
    match key.rsplit_once('@') {
        Some((name, marketplace)) => (name.to_owned(), marketplace.to_owned()),
        None => (key.to_owned(), String::new()),
    }
}

fn claude_installed(roots: &Roots) -> Vec<InstalledPlugin> {
    let Some(Value::Object(file)) = read_json(&roots.claude_plugins().join("installed_plugins.json"))
    else {
        return Vec::new();
    };
    let Some(Value::Object(plugins)) = file.get("plugins") else {
        return Vec::new();
    };
    plugins
        .iter()
        .filter_map(|(key, installs)| {
            let path = installs
                .as_array()?
                .iter()
                .rev()
                .find_map(|install| install.get("installPath")?.as_str().map(PathBuf::from))?;
            let (name, marketplace) = split_key(key);
            Some(InstalledPlugin {
                ecosystem: Ecosystem::Claude,
                contents: scan(&path),
                marketplace,
                name,
                enabled: true,
                path,
                in_jcode: false,
            })
        })
        .collect()
}

fn codex_config(roots: &Roots) -> toml::Table {
    fs::read_to_string(roots.codex_home().join("config.toml"))
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or_default()
}

/// Newest version folder of a Codex plugin in its cache.
fn codex_plugin_dir(roots: &Roots, marketplace: &str, name: &str) -> Option<PathBuf> {
    let base = roots.codex_home().join("plugins/cache").join(marketplace).join(name);
    fs::read_dir(base)
        .ok()?
        .flatten()
        .filter(|e| e.path().join(".codex-plugin/plugin.json").is_file())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

fn codex_installed(roots: &Roots) -> Vec<InstalledPlugin> {
    let config = codex_config(roots);
    let mut seen = std::collections::BTreeSet::new();
    let mut plugins = Vec::new();
    if let Some(toml::Value::Table(table)) = config.get("plugins") {
        for (key, value) in table {
            let (name, marketplace) = split_key(key);
            let enabled = value
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            if let Some(path) = codex_plugin_dir(roots, &marketplace, &name) {
                seen.insert((marketplace.clone(), name.clone()));
                plugins.push(InstalledPlugin {
                    ecosystem: Ecosystem::Codex,
                    contents: scan(&path),
                    marketplace,
                    name,
                    enabled,
                    path,
                    in_jcode: false,
                });
            }
        }
    }
    // Remote (curated) plugins live only in the cache, not in config.toml.
    let cache = roots.codex_home().join("plugins/cache");
    for marketplace in fs::read_dir(&cache).into_iter().flatten().flatten() {
        let marketplace_name = marketplace.file_name().to_string_lossy().into_owned();
        for plugin in fs::read_dir(marketplace.path()).into_iter().flatten().flatten() {
            let name = plugin.file_name().to_string_lossy().into_owned();
            if seen.contains(&(marketplace_name.clone(), name.clone())) {
                continue;
            }
            if let Some(path) = codex_plugin_dir(roots, &marketplace_name, &name) {
                let contents = scan(&path);
                plugins.push(InstalledPlugin {
                    ecosystem: Ecosystem::Codex,
                    name: contents.name.clone().unwrap_or(name),
                    contents,
                    marketplace: marketplace_name.clone(),
                    enabled: true,
                    path,
                    in_jcode: false,
                });
            }
        }
    }
    plugins
}

pub fn records(roots: &Roots) -> Vec<Record> {
    read_json(&roots.record_path())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn save_records(roots: &Roots, records: &[Record]) -> Result<()> {
    fs::create_dir_all(roots.plugins_dir())?;
    fs::write(roots.record_path(), serde_json::to_string_pretty(records)?)?;
    Ok(())
}

/// Everything installed, Claude Code first, then Codex, then plugins that
/// were installed into Jcode from a marketplace.
pub fn installed(roots: &Roots) -> Vec<InstalledPlugin> {
    let records = records(roots);
    let in_jcode = |key: &str| records.iter().any(|r| r.key == key);
    let mut all: Vec<InstalledPlugin> = claude_installed(roots)
        .into_iter()
        .chain(codex_installed(roots))
        .map(|mut p| {
            p.in_jcode = in_jcode(&p.key());
            p
        })
        .collect();
    all.sort_by(|a, b| (a.ecosystem as u8, &a.name).cmp(&(b.ecosystem as u8, &b.name)));
    for record in &records {
        if all.iter().any(|p| p.key() == record.key) {
            continue;
        }
        all.push(InstalledPlugin {
            ecosystem: Ecosystem::Jcode,
            marketplace: record.marketplace.clone(),
            name: record.name.clone(),
            enabled: true,
            contents: scan(&record.path),
            path: record.path.clone(),
            in_jcode: true,
        });
    }
    all
}

// ---------------------------------------------------------------------------
// Marketplaces

fn parse_source(root: &Path, source: &Value) -> Option<Source> {
    let local = |rel: &str| Source::Local(root.join(rel.trim_start_matches("./")));
    match source {
        Value::String(rel) => Some(local(rel)),
        Value::Object(obj) => {
            let field = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
            match field("source")?.as_str() {
                "local" => Some(local(&field("path")?)),
                "github" => Some(Source::Git {
                    url: format!("https://github.com/{}.git", field("repo")?),
                    subdir: field("path"),
                    git_ref: field("ref"),
                }),
                "url" | "git" | "git-subdir" => Some(Source::Git {
                    url: field("url")?,
                    subdir: field("path"),
                    git_ref: field("ref"),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Plugins listed by a marketplace checked out at `root`, in Claude Code
/// (`.claude-plugin/marketplace.json`) or Codex (`.agents/plugins/marketplace.json`) form.
pub fn read_marketplace(root: &Path, ecosystem: Ecosystem) -> Vec<MarketplacePlugin> {
    let Some(file) = [".claude-plugin/marketplace.json", ".agents/plugins/marketplace.json"]
        .iter()
        .find_map(|rel| read_json(&root.join(rel)))
    else {
        return Vec::new();
    };
    let marketplace = str_field(&file, "name")
        .or_else(|| Some(root.file_name()?.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let Some(list) = file.get("plugins").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|plugin| {
            let name = str_field(plugin, "name")?;
            let source = parse_source(root, plugin.get("source")?)?;
            let description = str_field(plugin, "description")
                .or_else(|| match &source {
                    Source::Local(dir) => scan(dir).description,
                    Source::Git { .. } => None,
                })
                .unwrap_or_default();
            Some(MarketplacePlugin {
                ecosystem,
                marketplace: marketplace.clone(),
                name,
                description,
                category: str_field(plugin, "category"),
                source,
                in_jcode: false,
            })
        })
        .collect()
}

/// Folders of every marketplace Claude Code, Codex or Jcode knows about.
fn marketplace_roots(roots: &Roots) -> Vec<(Ecosystem, PathBuf)> {
    let mut found = Vec::new();
    if let Some(Value::Object(known)) =
        read_json(&roots.claude_plugins().join("known_marketplaces.json"))
    {
        for entry in known.values() {
            if let Some(dir) = entry.get("installLocation").and_then(Value::as_str) {
                found.push((Ecosystem::Claude, PathBuf::from(dir)));
            }
        }
    }
    if let Some(toml::Value::Table(table)) = codex_config(roots).get("marketplaces") {
        for entry in table.values() {
            if let Some(dir) = entry.get("source").and_then(toml::Value::as_str) {
                // Codex writes verbatim `\\?\` paths.
                found.push((Ecosystem::Codex, PathBuf::from(dir.trim_start_matches(r"\\?\"))));
            }
        }
    }
    for entry in fs::read_dir(roots.jcode_home.join("marketplaces")).into_iter().flatten().flatten() {
        let dir = entry.path();
        let ecosystem = if dir.join(".agents/plugins/marketplace.json").is_file() {
            Ecosystem::Codex
        } else {
            Ecosystem::Claude
        };
        found.push((ecosystem, dir));
    }
    found
}

pub fn marketplace(roots: &Roots) -> Vec<MarketplacePlugin> {
    let records = records(roots);
    let mut plugins: Vec<_> = marketplace_roots(roots)
        .into_iter()
        .flat_map(|(ecosystem, dir)| read_marketplace(&dir, ecosystem))
        .map(|mut p| {
            p.in_jcode = records.iter().any(|r| r.key == p.key());
            p
        })
        .collect();
    plugins.sort_by(|a, b| a.name.cmp(&b.name).then(a.marketplace.cmp(&b.marketplace)));
    plugins
}

/// Clone a marketplace (for example `https://github.com/openai/plugins`)
/// into `~/.jcode/marketplaces/` so its plugins show up in [`marketplace`].
pub fn add_marketplace(roots: &Roots, url: &str) -> Result<PathBuf> {
    let name = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("-");
    if name.is_empty() || name.contains("..") {
        bail!("not a repository URL: {url}");
    }
    let dest = roots.jcode_home.join("marketplaces").join(&name);
    if dest.exists() {
        git(&["-C", &dest.to_string_lossy(), "pull", "--ff-only", "--quiet"])?;
    } else {
        fs::create_dir_all(dest.parent().unwrap())?;
        git(&["clone", "--depth", "1", "--quiet", url, &dest.to_string_lossy()])?;
    }
    if read_marketplace(&dest, Ecosystem::Claude).is_empty() {
        bail!("{url} has no .claude-plugin/marketplace.json or .agents/plugins/marketplace.json");
    }
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Installing into Jcode

fn git(args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .output()
        .context("git is not installed")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn safe_name(name: &str) -> Result<&str> {
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        bail!("unsafe plugin name: {name}");
    }
    Ok(name)
}

/// Bring the plugin's files to `dest`.
fn fetch(source: &Source, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    match source {
        Source::Local(dir) => copy_dir(dir, dest),
        Source::Git { url, subdir, git_ref } => {
            let checkout = tempfile::tempdir()?;
            let repo = checkout.path().join("repo");
            let repo_arg = repo.to_string_lossy().into_owned();
            let mut args = vec!["clone", "--depth", "1", "--quiet"];
            if let Some(r) = git_ref.as_deref() {
                args.extend(["--branch", r]);
            }
            args.extend([url.as_str(), repo_arg.as_str()]);
            git(&args)?;
            let from = match subdir {
                Some(sub) => repo.join(sub.trim_start_matches("./")),
                None => repo.clone(),
            };
            if !from.starts_with(&repo) || !from.is_dir() {
                bail!("{url} has no folder {}", subdir.as_deref().unwrap_or(""));
            }
            copy_dir(&from, dest)
        }
    }
}

fn load_mcp_config(roots: &Roots) -> Result<Map<String, Value>> {
    match fs::read_to_string(roots.mcp_path()) {
        Ok(text) => match serde_json::from_str(&text)? {
            Value::Object(map) => Ok(map),
            _ => Err(anyhow!("{} is not a JSON object", roots.mcp_path().display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e.into()),
    }
}

fn save_mcp_config(roots: &Roots, config: &Map<String, Value>) -> Result<()> {
    fs::create_dir_all(&roots.jcode_home)?;
    fs::write(roots.mcp_path(), serde_json::to_string_pretty(config)?)?;
    Ok(())
}

/// Replace plugin-root placeholders so stdio servers find their own files.
fn expand_root(value: &mut Value, root: &str) {
    match value {
        Value::String(s) => {
            for var in ["${CLAUDE_PLUGIN_ROOT}", "${PLUGIN_ROOT}", "${CODEX_PLUGIN_ROOT}"] {
                *s = s.replace(var, root);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| expand_root(v, root)),
        Value::Object(map) => map.values_mut().for_each(|v| expand_root(v, root)),
        _ => {}
    }
}

/// Install a plugin into Jcode from its marketplace `source`. Reinstalling
/// replaces what the previous install added.
pub fn install(
    roots: &Roots,
    ecosystem: Ecosystem,
    marketplace: &str,
    name: &str,
    source: &Source,
    mode: InstallMode,
) -> Result<InstallReport> {
    let key = format!("{name}@{marketplace}");
    if records(roots).iter().any(|r| r.key == key) {
        uninstall(roots, &key)?;
    }
    let dest = roots
        .plugins_dir()
        .join(safe_name(marketplace).unwrap_or("local"))
        .join(safe_name(name)?);
    fetch(source, &dest).with_context(|| format!("fetching {key}"))?;

    let manifest = manifest(&dest);
    let mut report = InstallReport::default();
    let skills_root = roots.skills_dir();
    fs::create_dir_all(&skills_root)?;
    for (skill, dir) in skill_dirs(&dest, manifest.as_ref()) {
        let target = skills_root.join(&skill);
        if target.exists() {
            report.skipped.push(format!("skill {skill}"));
            continue;
        }
        copy_dir(&dir, &target)?;
        report.skills.push(skill);
    }

    let servers = mcp_servers(&dest, manifest.as_ref());
    if mode == InstallMode::Full && !servers.is_empty() {
        let mut config = load_mcp_config(roots)?;
        let table = config
            .entry("servers")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| anyhow!("mcp.json \"servers\" is not an object"))?;
        let root = dest.to_string_lossy().into_owned();
        for (server, mut entry) in servers {
            let id = if table.contains_key(&server) {
                format!("{name}-{server}")
            } else {
                server.clone()
            };
            if table.contains_key(&id) {
                report.skipped.push(format!("MCP {server}"));
                continue;
            }
            expand_root(&mut entry, &root);
            if let Value::Object(obj) = &mut entry {
                obj.entry("enabled").or_insert(Value::Bool(true));
            }
            table.insert(id.clone(), entry);
            report.mcp_servers.push(id);
        }
        save_mcp_config(roots, &config)?;
    }

    let contents = scan(&dest);
    if contents.apps > 0 {
        report.unsupported.push(format!("{} Codex app(s)", contents.apps));
    }
    if contents.commands > 0 {
        report.unsupported.push(format!("{} slash command(s)", contents.commands));
    }
    if contents.agents > 0 {
        report.unsupported.push(format!("{} agent(s)", contents.agents));
    }

    let mut all = records(roots);
    all.push(Record {
        key,
        ecosystem,
        marketplace: marketplace.to_owned(),
        name: name.to_owned(),
        path: dest,
        skills: report.skills.clone(),
        mcp_servers: report.mcp_servers.clone(),
    });
    save_records(roots, &all)?;
    Ok(report)
}

/// Remove what [`install`] added for `key`, and nothing else.
pub fn uninstall(roots: &Roots, key: &str) -> Result<()> {
    let mut all = records(roots);
    let Some(index) = all.iter().position(|r| r.key == key) else {
        bail!("{key} is not installed in Jcode");
    };
    let record = all.remove(index);
    for skill in &record.skills {
        let dir = roots.skills_dir().join(safe_name(skill)?);
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
    }
    if !record.mcp_servers.is_empty() {
        let mut config = load_mcp_config(roots)?;
        if let Some(Value::Object(table)) = config.get_mut("servers") {
            for server in &record.mcp_servers {
                table.remove(server);
            }
        }
        save_mcp_config(roots, &config)?;
    }
    if record.path.starts_with(roots.plugins_dir()) && record.path.exists() {
        fs::remove_dir_all(&record.path)?;
    }
    save_records(roots, &all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn roots() -> (tempfile::TempDir, Roots) {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots {
            home: dir.path().join("home"),
            jcode_home: dir.path().join("home/.jcode"),
        };
        (dir, roots)
    }

    /// A Claude marketplace with one local plugin carrying a skill and a server.
    fn claude_marketplace(roots: &Roots) -> PathBuf {
        let market = roots.home.join(".claude/plugins/marketplaces/demo");
        write(
            &market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"demo","plugins":[
                {"name":"helper","description":"Helps","source":"./plugins/helper","category":"dev"},
                {"name":"remote","source":{"source":"git-subdir","url":"https://example.com/r.git","path":"plugins/x","ref":"v1"}}
            ]}"#,
        );
        let plugin = market.join("plugins/helper");
        write(&plugin.join(".claude-plugin/plugin.json"), r#"{"name":"helper","version":"1.0"}"#);
        write(&plugin.join("skills/review/SKILL.md"), "---\nname: review\n---\n");
        write(
            &plugin.join(".mcp.json"),
            r#"{"mcpServers":{"helper-db":{"command":"node","args":["${CLAUDE_PLUGIN_ROOT}/server.js"]}}}"#,
        );
        write(&plugin.join("commands/go.md"), "# go");
        write(
            &roots.home.join(".claude/plugins/known_marketplaces.json"),
            &serde_json::json!({"demo": {"installLocation": market}}).to_string(),
        );
        market
    }

    #[test]
    fn reads_claude_marketplace_sources() {
        let (_tmp, roots) = roots();
        let market = claude_marketplace(&roots);
        let plugins = marketplace(&roots);
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].name, "helper");
        assert_eq!(plugins[0].source, Source::Local(market.join("plugins/helper")));
        assert_eq!(plugins[0].category.as_deref(), Some("dev"));
        assert_eq!(
            plugins[1].source,
            Source::Git {
                url: "https://example.com/r.git".into(),
                subdir: Some("plugins/x".into()),
                git_ref: Some("v1".into()),
            }
        );
    }

    #[test]
    fn lists_claude_and_codex_installs() {
        let (_tmp, roots) = roots();
        let claude = roots.home.join(".claude/plugins/cache/demo/helper/abc");
        write(&claude.join(".claude-plugin/plugin.json"), r#"{"name":"helper","description":"Helps"}"#);
        write(&claude.join(".mcp.json"), r#"{"github":{"type":"http","url":"https://x"}}"#);
        write(
            &roots.home.join(".claude/plugins/installed_plugins.json"),
            &serde_json::json!({"version":2,"plugins":{"helper@demo":[{"installPath": claude}]}})
                .to_string(),
        );
        let codex = roots.home.join(".codex/plugins/cache/openai-bundled/browser/1.0");
        write(&codex.join(".codex-plugin/plugin.json"), r#"{"name":"browser","skills":"./skills/"}"#);
        write(&codex.join("skills/browse/SKILL.md"), "x");
        write(
            &roots.home.join(".codex/config.toml"),
            "[plugins.\"browser@openai-bundled\"]\nenabled = false\n",
        );

        let all = installed(&roots);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].ecosystem, Ecosystem::Claude);
        assert_eq!(all[0].contents.mcp_servers, ["github"]);
        assert_eq!(all[1].ecosystem, Ecosystem::Codex);
        assert!(!all[1].enabled);
        assert_eq!(all[1].contents.skills, ["browse"]);
    }

    #[test]
    fn install_and_uninstall_touch_only_their_own_entries() {
        let (_tmp, roots) = roots();
        claude_marketplace(&roots);
        // The user's own server and skill must survive.
        write(
            &roots.mcp_path(),
            r#"{"servers":{"mine":{"command":"x"},"helper-db":{"command":"y"}}}"#,
        );
        write(&roots.skills_dir().join("mine/SKILL.md"), "x");

        let entry = marketplace(&roots).into_iter().find(|p| p.name == "helper").unwrap();
        let report = install(
            &roots,
            entry.ecosystem,
            &entry.marketplace,
            &entry.name,
            &entry.source,
            InstallMode::Full,
        )
        .unwrap();
        assert_eq!(report.skills, ["review"]);
        // Name taken by the user's server, so it is namespaced.
        assert_eq!(report.mcp_servers, ["helper-helper-db"]);
        assert_eq!(report.unsupported, ["1 slash command(s)"]);

        let config = load_mcp_config(&roots).unwrap();
        let server = &config["servers"]["helper-helper-db"];
        let installed_dir = roots.plugins_dir().join("demo").join("helper");
        assert_eq!(
            server["args"][0],
            format!("{}/server.js", installed_dir.to_string_lossy())
        );
        assert!(roots.skills_dir().join("review/SKILL.md").is_file());
        assert!(marketplace(&roots)[0].in_jcode);

        uninstall(&roots, "helper@demo").unwrap();
        let config = load_mcp_config(&roots).unwrap();
        assert!(config["servers"].get("helper-helper-db").is_none());
        assert!(config["servers"].get("mine").is_some());
        assert!(config["servers"].get("helper-db").is_some());
        assert!(!roots.skills_dir().join("review").exists());
        assert!(roots.skills_dir().join("mine").exists());
        assert!(!installed_dir.exists());
        assert!(records(&roots).is_empty());
    }

    #[test]
    fn skills_only_install_leaves_mcp_alone() {
        let (_tmp, roots) = roots();
        claude_marketplace(&roots);
        let entry = marketplace(&roots).into_iter().find(|p| p.name == "helper").unwrap();
        let report = install(
            &roots,
            entry.ecosystem,
            &entry.marketplace,
            &entry.name,
            &entry.source,
            InstallMode::SkillsOnly,
        )
        .unwrap();
        assert_eq!(report.skills, ["review"]);
        assert!(report.mcp_servers.is_empty());
        assert!(!roots.mcp_path().exists());
    }

    /// Reads this machine's real Claude Code and Codex folders. Read-only.
    #[test]
    #[ignore]
    fn print_this_machine() {
        let roots = Roots::detect().unwrap();
        for p in installed(&roots) {
            println!(
                "{:<11} {:<28} skills={} mcp={:?} apps={}",
                p.ecosystem.label(),
                p.key(),
                p.contents.skills.len(),
                p.contents.mcp_servers,
                p.contents.apps
            );
        }
        let market = marketplace(&roots);
        println!("marketplace entries: {}", market.len());
        for (eco, n) in [Ecosystem::Claude, Ecosystem::Codex]
            .map(|e| (e, market.iter().filter(|p| p.ecosystem == e).count()))
        {
            println!("  {}: {n}", eco.label());
        }
    }

    #[test]
    fn rejects_path_traversal_names() {
        let (_tmp, roots) = roots();
        let err = install(
            &roots,
            Ecosystem::Claude,
            "demo",
            "../evil",
            &Source::Local(roots.home.clone()),
            InstallMode::Full,
        );
        assert!(err.is_err());
    }
}
