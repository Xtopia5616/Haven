use std::{
    borrow::Cow,
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use tauri::{
    App, Assets, Manager, Runtime,
    path::BaseDirectory,
    utils::assets::{AssetKey, AssetsIter, CspHash},
};

const UI_RESOURCE_DIRECTORY: &str = "ui";
const INDEX_HTML: &str = "index.html";
const CSP_HASHES_FILE: &str = ".haven-csp-hashes.json";
const CSP_HASHES_ASSET_KEY: &str = "/.haven-csp-hashes.json";
const MAX_CSP_HASHES: usize = 128;
const MAX_CSP_HASH_MANIFEST_BYTES: u64 = 16 * 1024;

#[derive(Clone, Default)]
pub(crate) struct ResourceUiAssets {
    inner: Arc<ResourceUiAssetsInner>,
}

#[derive(Default)]
struct ResourceUiAssetsInner {
    root: OnceLock<PathBuf>,
    script_hashes: OnceLock<Vec<String>>,
}

impl ResourceUiAssets {
    pub(crate) fn is_ready(&self) -> bool {
        self.inner.root.get().is_some() && self.inner.script_hashes.get().is_some()
    }

    fn root(&self) -> Option<&Path> {
        self.inner.root.get().map(PathBuf::as_path)
    }

    fn read_asset(&self, key: &AssetKey) -> Option<Vec<u8>> {
        if key.as_ref() == CSP_HASHES_ASSET_KEY {
            return None;
        }

        let path = resolve_asset_path(self.root()?, key)?;
        fs::read(path).ok()
    }

    #[cfg(test)]
    fn from_root(root: PathBuf) -> Self {
        let assets = Self::default();
        let root = canonical_ui_root(root).expect("test UI resource root exists");
        let script_hashes = read_csp_hashes(&root).expect("test CSP hash manifest exists");
        assets.inner.root.set(root).expect("root is unset");
        assets
            .inner
            .script_hashes
            .set(script_hashes)
            .expect("script hashes are unset");
        assets
    }
}

impl<R: Runtime> Assets<R> for ResourceUiAssets {
    fn setup(&self, app: &App<R>) {
        if tauri::is_dev() {
            return;
        }

        let resource_root = app
            .path()
            .resolve(UI_RESOURCE_DIRECTORY, BaseDirectory::Resource)
            .ok()
            .and_then(canonical_ui_root);

        let Some(root) = resource_root else {
            tracing::error!("failed to locate the external UI resources in the application bundle");
            return;
        };

        let Some(script_hashes) = read_csp_hashes(&root) else {
            tracing::error!("failed to read the external UI Content Security Policy hash manifest");
            return;
        };

        if self.inner.root.set(root).is_err()
            || self.inner.script_hashes.set(script_hashes).is_err()
        {
            tracing::error!("external UI resources were initialized more than once");
        }
    }

    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        self.read_asset(key).map(Cow::Owned)
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        let assets = self.root().map(collect_assets).unwrap_or_default();
        Box::new(assets.into_iter())
    }

    fn csp_hashes(&self, html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        if !html_path.as_ref().ends_with(".html") {
            return Box::new(std::iter::empty());
        }

        let hashes = self
            .inner
            .script_hashes
            .get()
            .map(Vec::as_slice)
            .unwrap_or_default();
        Box::new(hashes.iter().map(|hash| CspHash::Script(hash.as_str())))
    }
}

fn canonical_ui_root(root: PathBuf) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    (root.join(INDEX_HTML).is_file() && root.join(CSP_HASHES_FILE).is_file()).then_some(root)
}

fn read_csp_hashes(root: &Path) -> Option<Vec<String>> {
    let root = root.canonicalize().ok()?;
    let path = root.join(CSP_HASHES_FILE).canonicalize().ok()?;
    if !path.starts_with(root) {
        return None;
    }
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CSP_HASH_MANIFEST_BYTES {
        return None;
    }

    let hashes: Vec<String> = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    if hashes.len() > MAX_CSP_HASHES
        || hashes.iter().any(|hash| {
            hash.strip_prefix("'sha256-")
                .and_then(|hash| hash.strip_suffix('\''))
                .and_then(|hash| STANDARD.decode(hash).ok())
                .is_none_or(|hash| hash.len() != 32)
        })
    {
        return None;
    }

    Some(hashes)
}

fn resolve_asset_path(root: &Path, key: &AssetKey) -> Option<PathBuf> {
    let relative = key.as_ref().strip_prefix('/')?;
    let relative_path = Path::new(relative);
    if relative_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }

    let path = root.join(relative_path).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

type OwnedAsset = (Cow<'static, str>, Cow<'static, [u8]>);

fn collect_assets(root: &Path) -> Vec<OwnedAsset> {
    let mut assets = Vec::new();
    collect_assets_from_directory(root, root, &mut assets);
    assets
}

fn collect_assets_from_directory(root: &Path, directory: &Path, assets: &mut Vec<OwnedAsset>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            collect_assets_from_directory(root, &path, assets);
            continue;
        }
        if !file_type.is_file() || path.file_name().is_some_and(|name| name == CSP_HASHES_FILE) {
            continue;
        }

        let (Ok(relative), Ok(bytes)) = (path.strip_prefix(root), fs::read(&path)) else {
            continue;
        };
        let key = format!("/{}", relative.to_string_lossy().replace('\\', "/"));
        assets.push((Cow::Owned(key), Cow::Owned(bytes)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, hashes: &[String]) {
        fs::write(
            root.join(CSP_HASHES_FILE),
            serde_json::to_vec(hashes).expect("serialize CSP hashes"),
        )
        .expect("write CSP hash manifest");
    }

    #[test]
    fn serves_external_assets_and_rejects_parent_paths() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().join("ui");
        fs::create_dir(&root).expect("create UI resource root");
        fs::write(
            root.join(INDEX_HTML),
            "<html><head></head><body>ready</body></html>",
        )
        .expect("write html");
        fs::write(root.join("app.css"), "body { color: red; }").expect("write css");
        fs::write(directory.path().join("outside.txt"), "outside")
            .expect("write file outside UI resource root");
        write_manifest(&root, &[]);

        let assets = ResourceUiAssets::from_root(root);
        let css_key = AssetKey::from("app.css");
        let escaped_key = AssetKey::from("../outside.txt");
        let manifest_key = AssetKey::from(CSP_HASHES_FILE);

        assert_eq!(
            <ResourceUiAssets as Assets<tauri::Wry>>::get(&assets, &css_key).as_deref(),
            Some(b"body { color: red; }".as_slice())
        );
        assert!(<ResourceUiAssets as Assets<tauri::Wry>>::get(&assets, &escaped_key).is_none());
        assert!(<ResourceUiAssets as Assets<tauri::Wry>>::get(&assets, &manifest_key).is_none());
    }

    #[test]
    fn reads_only_valid_bounded_csp_hashes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let valid_hash = format!("'sha256-{}'", STANDARD.encode([7_u8; 32]));
        write_manifest(directory.path(), std::slice::from_ref(&valid_hash));
        assert_eq!(
            read_csp_hashes(directory.path()),
            Some(vec![valid_hash.clone()])
        );

        write_manifest(directory.path(), &["not-a-hash".to_string()]);
        assert!(read_csp_hashes(directory.path()).is_none());

        let too_many_hashes = vec![valid_hash; MAX_CSP_HASHES + 1];
        write_manifest(directory.path(), &too_many_hashes);
        assert!(read_csp_hashes(directory.path()).is_none());
    }

    #[test]
    fn iterates_external_files_but_hides_the_csp_manifest() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join(INDEX_HTML), "<html></html>").expect("write html");
        fs::create_dir(directory.path().join("assets")).expect("create asset directory");
        fs::write(directory.path().join("assets/app.css"), "body {}").expect("write css");
        write_manifest(directory.path(), &[]);

        let assets = ResourceUiAssets::from_root(directory.path().to_path_buf());
        let files = <ResourceUiAssets as Assets<tauri::Wry>>::iter(&assets)
            .map(|(path, _)| path.into_owned())
            .collect::<Vec<_>>();

        assert!(files.contains(&"/index.html".to_string()));
        assert!(files.contains(&"/assets/app.css".to_string()));
        assert!(!files.contains(&format!("/{CSP_HASHES_FILE}")));
    }
}
