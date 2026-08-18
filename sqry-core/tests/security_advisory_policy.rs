//! Regression guards for dependency advisory floors and exception policy.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use semver::{Version, VersionReq};
use toml::Value;

const REMOVED_RSA_ADVISORY: &str = "RUSTSEC-2023-0071";
const VULNERABLE_LRU_VERSION: &str = "0.18.1";
const MINIMUM_LRU_VERSION: &str = "0.18.2";
const MINIMUM_EVENT_LISTENER_VERSION: &str = "5.4.2";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sqry-core must be a direct workspace member")
        .to_path_buf()
}

fn read_toml(path: &Path) -> Result<Value> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str::<Value>(&source)
        .with_context(|| format!("failed to parse {} as TOML", path.display()))
}

fn advisory_ignore_set(path: &Path) -> Result<BTreeSet<String>> {
    let document = read_toml(path)?;
    let ignore = document
        .get("advisories")
        .and_then(|value| value.get("ignore"))
        .and_then(Value::as_array)
        .with_context(|| {
            format!(
                "{} must define advisories.ignore as an array",
                path.display()
            )
        })?;

    let mut normalized = BTreeSet::new();
    for entry in ignore {
        let advisory = entry.as_str().with_context(|| {
            format!(
                "{} advisories.ignore entries must be strings",
                path.display()
            )
        })?;
        ensure!(
            normalized.insert(advisory.to_owned()),
            "{} contains duplicate advisory exception {advisory}",
            path.display()
        );
    }
    Ok(normalized)
}

fn is_dependency_table(name: &str) -> bool {
    matches!(
        name,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    )
}

fn is_lru_dependency(name: &str, spec: &Value) -> bool {
    name == "lru"
        || spec
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(Value::as_str)
            .is_some_and(|package| package == "lru")
}

fn collect_lru_declarations(
    value: &Value,
    manifest: &str,
    path: &mut Vec<String>,
    declarations: &mut Vec<(String, Value)>,
) {
    let Some(table) = value.as_table() else {
        return;
    };

    for (key, child) in table {
        path.push(key.clone());
        if is_dependency_table(key)
            && let Some(dependencies) = child.as_table()
        {
            for (dependency_name, spec) in dependencies {
                if is_lru_dependency(dependency_name, spec) {
                    let location = format!("{manifest}:{}.{}", path.join("."), dependency_name);
                    declarations.push((location, spec.clone()));
                }
            }
        }
        collect_lru_declarations(child, manifest, path, declarations);
        path.pop();
    }
}

fn effective_requirement(spec: &Value, workspace_requirement: &VersionReq) -> Result<VersionReq> {
    if let Some(requirement) = spec.as_str() {
        return VersionReq::parse(requirement)
            .with_context(|| format!("invalid lru version requirement {requirement:?}"));
    }

    let table = spec
        .as_table()
        .context("lru dependency must be a string or table")?;
    if table
        .get("workspace")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        ensure!(
            table.get("version").is_none(),
            "workspace lru dependency must not override version"
        );
        return Ok(workspace_requirement.clone());
    }

    let requirement = table
        .get("version")
        .and_then(Value::as_str)
        .context("non-workspace lru dependency must declare a version")?;
    VersionReq::parse(requirement)
        .with_context(|| format!("invalid lru version requirement {requirement:?}"))
}

fn lock_packages<'a>(lock: &'a Value, name: &str) -> Result<Vec<&'a toml::Table>> {
    let packages = lock
        .get("package")
        .and_then(Value::as_array)
        .context("Cargo.lock must contain a package array")?;
    Ok(packages
        .iter()
        .filter_map(Value::as_table)
        .filter(|package| package.get("name").and_then(Value::as_str) == Some(name))
        .collect())
}

fn locked_version(package: &toml::Table, name: &str) -> Result<Version> {
    let version = package
        .get("version")
        .and_then(Value::as_str)
        .with_context(|| format!("locked {name} package must have a string version"))?;
    Version::parse(version).with_context(|| format!("locked {name} version is invalid: {version}"))
}

#[test]
fn rustsec_ignore_sets_are_equal_and_obsolete_rsa_exception_is_absent() -> Result<()> {
    let root = workspace_root();
    let deny_ignores = advisory_ignore_set(&root.join("deny.toml"))?;
    let audit_ignores = advisory_ignore_set(&root.join(".cargo/audit.toml"))?;

    ensure!(
        deny_ignores == audit_ignores,
        "deny.toml and .cargo/audit.toml advisory ignore sets differ"
    );
    ensure!(
        !deny_ignores.contains(REMOVED_RSA_ADVISORY),
        "obsolete {REMOVED_RSA_ADVISORY} exception must not return"
    );
    Ok(())
}

#[test]
fn every_lru_declaration_enforces_the_patched_floor() -> Result<()> {
    let root = workspace_root();
    let root_manifest = read_toml(&root.join("Cargo.toml"))?;
    let members = root_manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
        .context("root Cargo.toml must define workspace.members")?;

    let mut manifests = vec![("Cargo.toml".to_owned(), root_manifest.clone())];
    for member in members {
        let member = member
            .as_str()
            .context("workspace member paths must be strings")?;
        ensure!(
            !member.contains(['*', '?', '[', ']']),
            "workspace member globs are unsupported by this fail-closed guard: {member}"
        );
        let relative_manifest = format!("{member}/Cargo.toml");
        manifests.push((
            relative_manifest.clone(),
            read_toml(&root.join(relative_manifest))?,
        ));
    }

    let mut declarations = Vec::new();
    for (manifest, document) in &manifests {
        collect_lru_declarations(document, manifest, &mut Vec::new(), &mut declarations);
    }

    let discovered_manifests: BTreeSet<&str> = declarations
        .iter()
        .map(|(location, _)| location.split(':').next().unwrap_or_default())
        .collect();
    let expected_manifests = BTreeSet::from([
        "Cargo.toml",
        "sqry-classpath/Cargo.toml",
        "sqry-cli/Cargo.toml",
        "sqry-core/Cargo.toml",
        "sqry-mcp/Cargo.toml",
    ]);
    ensure!(
        discovered_manifests == expected_manifests,
        "unexpected lru declaration manifest set: {discovered_manifests:?}"
    );
    ensure!(
        declarations.len() == expected_manifests.len(),
        "each expected manifest must contain exactly one lru declaration"
    );

    let workspace_spec = root_manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(|dependencies| dependencies.get("lru"))
        .context("workspace.dependencies.lru must exist")?;
    let workspace_requirement = effective_requirement(workspace_spec, &VersionReq::STAR)?;
    let vulnerable = Version::parse(VULNERABLE_LRU_VERSION)?;

    let lock = read_toml(&root.join("Cargo.lock"))?;
    let lru_packages = lock_packages(&lock, "lru")?;
    ensure!(
        lru_packages.len() == 1,
        "Cargo.lock must contain exactly one lru package"
    );
    let locked = locked_version(lru_packages[0], "lru")?;

    for (location, spec) in declarations {
        let requirement = effective_requirement(&spec, &workspace_requirement)
            .with_context(|| format!("invalid declaration at {location}"))?;
        ensure!(
            !requirement.matches(&vulnerable),
            "{location} still accepts vulnerable lru {vulnerable}"
        );
        ensure!(
            requirement.matches(&locked),
            "{location} does not accept locked lru {locked}"
        );
    }
    Ok(())
}

#[test]
fn lock_contains_only_patched_advisory_packages() -> Result<()> {
    let lock = read_toml(&workspace_root().join("Cargo.lock"))?;

    let lru_packages = lock_packages(&lock, "lru")?;
    ensure!(
        lru_packages.len() == 1,
        "Cargo.lock must contain exactly one lru package"
    );
    let lru = locked_version(lru_packages[0], "lru")?;
    ensure!(
        lru >= Version::parse(MINIMUM_LRU_VERSION)?,
        "locked lru {lru} is vulnerable"
    );

    let event_listener_packages = lock_packages(&lock, "event-listener")?;
    ensure!(
        event_listener_packages.len() == 1,
        "Cargo.lock must contain exactly one event-listener package"
    );
    let event_listener = locked_version(event_listener_packages[0], "event-listener")?;
    ensure!(
        event_listener >= Version::parse(MINIMUM_EVENT_LISTENER_VERSION)?,
        "locked event-listener {event_listener} is vulnerable"
    );

    let event_dependencies = event_listener_packages[0]
        .get("dependencies")
        .and_then(Value::as_array)
        .context("locked event-listener must have a dependency array")?;
    ensure!(
        event_dependencies.iter().all(|dependency| {
            dependency
                .as_str()
                .is_none_or(|name| !name.starts_with("concurrent-queue"))
        }),
        "patched event-listener must not depend on concurrent-queue"
    );
    ensure!(
        lock_packages(&lock, "concurrent-queue")?.is_empty(),
        "unreferenced concurrent-queue must not remain locked"
    );
    ensure!(
        lock_packages(&lock, "rsa")?.is_empty(),
        "rsa must not remain locked"
    );
    Ok(())
}
