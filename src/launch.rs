use std::{
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use native_dialog::{FileDialog, MessageDialog, MessageType};
use serde::{Deserialize, Serialize};

use crate::cli::AppArgs;

const PACKAGE_SOURCE_FILE: &str = "package-source.toml";
const DESTINY2_BASE_DEPOT_ID: &str = "1085661";

#[derive(Debug, Serialize, Deserialize)]
struct PackageSourceConfig {
    packages_path: PathBuf,
}

/// Normal GUI control flow: the user closed the package picker before choosing
/// a corpus.  This is deliberately distinct from a startup failure so an
/// Explorer launch can return quietly.
#[derive(Debug)]
pub struct PackageSelectionCancelled;

impl fmt::Display for PackageSelectionCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Package source selection cancelled")
    }
}

impl std::error::Error for PackageSelectionCancelled {}

pub fn resolve_package_source(args: &AppArgs) -> anyhow::Result<PathBuf> {
    if let Some(command) = args.command.as_ref() {
        let explicit = explicit_source(args).ok_or_else(|| {
            anyhow!(
                "{} requires an explicit --gamedir <client-root> or --packages \
                 <packages-directory>",
                command.name()
            )
        })?;
        return normalize_explicit_source(args, explicit);
    }

    if let Some(source) = explicit_source(args) {
        return normalize_explicit_source(args, source);
    }

    if let Some(source) = load_remembered_source()? {
        if let Ok(normalized) = normalize_picker_selection(&source) {
            return Ok(normalized);
        }
        show_alert(
            "Saved package source is unavailable",
            &format!(
                "The saved Pre-BL package source no longer exists:\n{}\n\nSelect the preserved \
                 Arrivals client root or packages directory.",
                source.display()
            ),
        );
    }

    loop {
        let Some(selection) = FileDialog::new()
            .set_title("Select Shadowkeep / Arrivals packages")
            .show_open_single_dir()
            .context("Opening package directory picker")?
        else {
            return Err(PackageSelectionCancelled.into());
        };

        match normalize_picker_selection(&selection) {
            Ok(path) => return Ok(path),
            Err(error) => show_alert("Invalid package source", &format!("{error:#}")),
        }
    }
}

pub fn remember_package_source(packages_path: &Path) -> anyhow::Result<()> {
    let config = PackageSourceConfig {
        packages_path: packages_path.to_path_buf(),
    };
    let serialized = toml::to_string_pretty(&config).context("Serializing package source")?;
    alkahest_core::atomic_write(
        &alkahest_core::config_relative_path(PACKAGE_SOURCE_FILE),
        serialized.as_bytes(),
    )
}

pub fn show_startup_error(error: &anyhow::Error) {
    show_alert(
        "Alkahest Pre-BL could not start",
        &format!(
            "{error:#}\n\nDiagnostics: {}",
            alkahest_core::data_relative_path("alkahest-prebl.log").display()
        ),
    );
}

fn explicit_source(args: &AppArgs) -> Option<PathBuf> {
    args.gamedir
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| args.packages.as_ref().map(PathBuf::from))
}

fn normalize_explicit_source(args: &AppArgs, source: PathBuf) -> anyhow::Result<PathBuf> {
    if args.gamedir.is_some() {
        normalize_client_or_download_root(&source)
    } else if directory_contains_packages(&source)? {
        normalize_packages_dir(&source)
    } else {
        // Be forgiving when a DepotDownloader root is supplied to --packages.
        normalize_client_or_download_root(&source)
    }
}

fn load_remembered_source() -> anyhow::Result<Option<PathBuf>> {
    let path = alkahest_core::config_relative_path(PACKAGE_SOURCE_FILE);
    if !path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Reading saved package source {}", path.display()))?;
    let config: PackageSourceConfig = toml::from_str(&contents)
        .with_context(|| format!("Parsing saved package source {}", path.display()))?;
    Ok(Some(config.packages_path))
}

fn normalize_picker_selection(selection: &Path) -> anyhow::Result<PathBuf> {
    let is_packages = selection
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("packages"));
    if is_packages {
        normalize_packages_dir(selection)
    } else {
        normalize_client_or_download_root(selection)
    }
}

fn normalize_client_or_download_root(path: &Path) -> anyhow::Result<PathBuf> {
    let direct_packages = path.join("packages");
    if direct_packages.is_dir() {
        return normalize_packages_dir(&direct_packages);
    }

    if let Some(packages) = find_depotdownloader_packages(path)? {
        return normalize_packages_dir(&packages);
    }

    anyhow::bail!(
        "No Destiny 2 packages were found under {}. Select a client root, its packages folder, or a DepotDownloader root containing depots\\{}\\<install>\\packages.",
        path.display(),
        DESTINY2_BASE_DEPOT_ID
    )
}

fn find_depotdownloader_packages(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let leaf = path.file_name().and_then(|name| name.to_str());
    let base_depot = if leaf.is_some_and(|name| name == DESTINY2_BASE_DEPOT_ID) {
        path.to_path_buf()
    } else if leaf.is_some_and(|name| name.eq_ignore_ascii_case("depots")) {
        path.join(DESTINY2_BASE_DEPOT_ID)
    } else {
        path.join("depots").join(DESTINY2_BASE_DEPOT_ID)
    };

    if !base_depot.is_dir() {
        return Ok(None);
    }

    let mut candidates = std::fs::read_dir(&base_depot)
        .with_context(|| format!("Reading DepotDownloader depot {}", base_depot.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|install| install.join("packages").is_dir())
        .collect::<Vec<_>>();

    // DepotDownloader can retain more than one manifest. Its current install is
    // the most recently updated directory; use the path as a stable tie-breaker.
    candidates.sort_by(|a, b| {
        let modified = |path: &Path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        };
        modified(b).cmp(&modified(a)).then_with(|| b.cmp(a))
    });

    Ok(candidates
        .into_iter()
        .next()
        .map(|install| install.join("packages")))
}

fn normalize_packages_dir(path: &Path) -> anyhow::Result<PathBuf> {
    let path = std::fs::canonicalize(path)
        .with_context(|| format!("Packages directory does not exist: {}", path.display()))?;
    if !path.is_dir() {
        anyhow::bail!("Packages path is not a directory: {}", path.display());
    }
    if !directory_contains_packages(&path)? {
        anyhow::bail!("No .pkg files were found in packages directory: {}", path.display());
    }
    Ok(path)
}

fn directory_contains_packages(path: &Path) -> anyhow::Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }

    Ok(std::fs::read_dir(path)
        .with_context(|| format!("Reading package source {}", path.display()))?
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pkg"))
        }))
}

fn show_alert(title: &str, text: &str) {
    if let Err(error) = MessageDialog::new()
        .set_type(MessageType::Error)
        .set_title(title)
        .set_text(text)
        .show_alert()
    {
        eprintln!("Failed to show dialog: {error}");
    }
}

trait CommandName {
    fn name(&self) -> &'static str;
}

impl CommandName for crate::cli::AppCommand {
    fn name(&self) -> &'static str {
        match self {
            crate::cli::AppCommand::Export { .. } => "export",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_accepts_packages_leaf_case_insensitively() {
        assert!(
            Path::new("C:/game/PACKAGES")
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("packages"))
        );
    }

    #[test]
    fn depotdownloader_root_resolves_base_depot_packages() {
        let root = std::env::temp_dir().join(format!(
            "alkahest-depot-source-test-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let packages = root
            .join("depots")
            .join(DESTINY2_BASE_DEPOT_ID)
            .join("24238629")
            .join("packages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(packages.join("w64_test_0000_0.pkg"), []).unwrap();

        let resolved = normalize_picker_selection(&root).unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&packages).unwrap());

        std::fs::remove_dir_all(root).unwrap();
    }
}
