use std::{
    any::TypeId,
    sync::{Arc, atomic::AtomicUsize},
    time::{Duration, Instant},
};

use alkahest_core::job::{SCHEDULER, potassium::Priority};
use alkahest_data::tag::WideHash;
use hashbrown::HashMap;
use parking_lot::Mutex;
use tiger_pkg::TagHash;
use uuid::Uuid;

use super::{
    Asset,
    handle::{Handle, UntypedHandle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Texture,
    Technique,
    VertexBuffer,
    IndexBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFallback {
    GenericWhite,
    NeutralLookup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetLoadState {
    Queued,
    Loading,
    Ready,
    Failed { error: String },
    Fallback { fallback: TextureFallback, error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetDiagnostic {
    pub tag: TagHash,
    pub kind: AssetKind,
    pub state: AssetLoadState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AssetDiagnosticSummary {
    pub queued: usize,
    pub loading: usize,
    pub ready: usize,
    pub failed: usize,
    pub fallback: usize,
}
use crate::{
    Gpu,
    asset::{
        index_buffer::{IndexBuffer, load_index_buffer},
        technique::Technique,
        texture::Texture,
        vertex_buffer::{VertexBuffer, load_vertex_buffer},
    },
};

// Asynchronous asset manager. Allows taking a handle to an ArcShift<Option<T>> (where T: Asset), which will be populated with the asset once it is loaded.
// Works for any asset type that implements the Asset trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetEra {
    Current,
    Shadowkeep,
}

/// Shared, era-aware asset cache.  Techniques are special: their dynamic
/// constants recursively request textures, so the background loader must keep
/// the same manager alive instead of using the former detached loader.
#[derive(Clone)]
pub struct AssetManager {
    gpu: Arc<Gpu>,
    pub assets: Arc<Mutex<HashMap<TagHash, (TypeId, UntypedHandle)>>>,
    num_loading: Arc<AtomicUsize>,
    dummy_handle: UntypedHandle,
    last_load: Arc<Mutex<Instant>>,
    era: AssetEra,
    diagnostics: Arc<Mutex<HashMap<TagHash, AssetDiagnostic>>>,
}

impl AssetManager {
    pub fn new(gpu: &Arc<Gpu>) -> Self {
        Self::new_for_era(gpu, AssetEra::Current)
    }

    pub fn new_shadowkeep(gpu: &Arc<Gpu>) -> Self {
        Self::new_for_era(gpu, AssetEra::Shadowkeep)
    }

    fn new_for_era(gpu: &Arc<Gpu>, era: AssetEra) -> Self {
        Self {
            gpu: gpu.clone(),
            assets: Arc::new(Mutex::new(HashMap::new())),
            num_loading: Arc::new(AtomicUsize::new(0)),
            dummy_handle: UntypedHandle::new(TagHash::NONE),
            last_load: Arc::new(Mutex::new(Instant::now())),
            era,
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn era(&self) -> AssetEra {
        self.era
    }

    pub fn get<T: Asset + 'static>(&self, tag: TagHash) -> Option<Handle<T>> {
        if let Some((ty, handle)) = self.assets.lock().get(&tag)
            && *ty == TypeId::of::<T>()
        {
            return Some(unsafe { handle.clone_as_typed_unchecked::<T>() });
        }
        None
    }

    pub fn load<T: Asset + 'static>(&self, tag: impl Into<WideHash>) -> Handle<T> {
        self.try_load(tag)
            .unwrap_or_else(|| unsafe { self.dummy_handle.clone_as_typed_unchecked() })
    }

    /// Get the asset handle for the given tag, or create a new one, and send it to the loader thread.
    /// Returns None if the tag is null
    #[profiling::function]
    pub fn try_load<T: Asset + 'static>(&self, tag: impl Into<WideHash>) -> Option<Handle<T>> {
        let tag = tag.into().hash32();
        if tag.is_none() {
            // TODO: Return a dummy handle instead of None
            return None;
        }

        let mut cache = self.assets.lock();
        if let Some((ty, handle)) = cache.get(&tag) {
            if *ty == TypeId::of::<T>() {
                return Some(unsafe { handle.clone_as_typed_unchecked::<T>() });
            } else {
                error!("AssetManager::try_load: Tag {tag} already loaded with different type");
                return None;
            }
        }

        let handle = UntypedHandle::new(tag);
        cache.insert(tag, (TypeId::of::<T>(), handle.clone()));
        drop(cache);

        self.num_loading
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_load.lock() = Instant::now();

        let request = LoadRequest {
            tag,
            handle: handle.clone(),
            type_id: T::ASSET_TYPE,
        };
        self.set_diagnostic(tag, asset_kind(T::ASSET_TYPE), AssetLoadState::Queued);
        let gpu = self.gpu.clone();
        let num_loaded = self.num_loading.clone();
        let manager = self.clone();
        SCHEDULER
            .job_builder("load_asset")
            .priority(Priority::Low)
            .spawn(move || {
                load_asset(request, &gpu, &num_loaded, &manager);
            });

        // SAFETY: The type ID was checked above
        Some(unsafe { handle.clone_as_typed_unchecked() })
    }

    /// Cull assets that are no longer referenced (ref count == 1, since the asset handle itself holds a reference)
    #[profiling::function]
    pub fn remove_unreferenced(&self) {
        self.assets.lock().retain(|t, (_, handle)| {
            if handle.ref_count() == 1 {
                debug!("Culling asset {t}");
            }
            handle.ref_count() > 1
        });
    }

    pub fn count_loading(&self) -> usize {
        self.num_loading.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn time_since_last_load(&self) -> Duration {
        self.last_load.lock().elapsed()
    }

    pub fn is_idle(&self) -> bool {
        self.count_loading() == 0 && self.time_since_last_load() > Duration::from_millis(500)
    }

    pub fn diagnostics(&self) -> Vec<AssetDiagnostic> {
        let mut diagnostics = self.diagnostics.lock().values().cloned().collect::<Vec<_>>();
        diagnostics.sort_unstable_by_key(|diagnostic| diagnostic.tag.0);
        diagnostics
    }

    pub fn diagnostic_summary(&self) -> AssetDiagnosticSummary {
        let mut summary = AssetDiagnosticSummary::default();
        for diagnostic in self.diagnostics.lock().values() {
            match diagnostic.state {
                AssetLoadState::Queued => summary.queued += 1,
                AssetLoadState::Loading => summary.loading += 1,
                AssetLoadState::Ready => summary.ready += 1,
                AssetLoadState::Failed { .. } => summary.failed += 1,
                AssetLoadState::Fallback { .. } => summary.fallback += 1,
            }
        }
        summary
    }

    fn set_diagnostic(&self, tag: TagHash, kind: AssetKind, state: AssetLoadState) {
        self.diagnostics.lock().insert(tag, AssetDiagnostic { tag, kind, state });
    }

    /// Record an intentional semantic fallback without changing the failed
    /// asset's causal error.  Callers use this only for optional inputs whose
    /// absence is established by the era bootstrap.
    pub fn record_fallback(&self, tag: TagHash, fallback: TextureFallback, error: impl Into<String>) {
        self.set_diagnostic(
            tag,
            AssetKind::Texture,
            AssetLoadState::Fallback {
                fallback,
                error: error.into(),
            },
        );
    }
}

fn asset_kind(type_id: Uuid) -> AssetKind {
    match type_id {
        Texture::ASSET_TYPE => AssetKind::Texture,
        Technique::ASSET_TYPE => AssetKind::Technique,
        VertexBuffer::ASSET_TYPE => AssetKind::VertexBuffer,
        IndexBuffer::ASSET_TYPE => AssetKind::IndexBuffer,
        _ => unreachable!("unknown asset type {type_id}"),
    }
}

struct LoadRequest {
    tag: TagHash,
    handle: UntypedHandle,
    type_id: Uuid,
}

fn load_asset(
    request: LoadRequest,
    gpu: &Arc<Gpu>,
    num_loaded: &Arc<AtomicUsize>,
    manager: &AssetManager,
) {
    let kind = asset_kind(request.type_id);
    manager.set_diagnostic(request.tag, kind, AssetLoadState::Loading);
    match request.type_id {
        Texture::ASSET_TYPE => {
            let result = match manager.era() {
                AssetEra::Current => Texture::load(&gpu.device, request.tag),
                AssetEra::Shadowkeep => Texture::load_shadowkeep(&gpu.device, request.tag),
            };
            match result {
                Ok(o) => {
                    request.handle.update(o.into());
                    manager.set_diagnostic(request.tag, kind, AssetLoadState::Ready);
                }
                Err(e) => {
                    let error = format!("{e:#}");
                    error!("Failed to load {:?} texture {}: {error}", manager.era(), request.tag);
                    // Do not turn a present-but-misdecoded required texture into a
                    // plausible-looking white material.  A caller may apply an
                    // explicit semantic fallback only after classifying its slot as
                    // optional; until then the causal failure remains observable.
                    manager.set_diagnostic(request.tag, kind, AssetLoadState::Failed { error });
                }
            }
        }
        VertexBuffer::ASSET_TYPE => match load_vertex_buffer(gpu, request.tag) {
            Ok(o) => {
                request.handle.update(o.into());
                manager.set_diagnostic(request.tag, kind, AssetLoadState::Ready);
            }
            Err(e) => {
                let error = format!("{e:#}");
                error!("Failed to load vertex buffer {}: {error}", request.tag);
                manager.set_diagnostic(request.tag, kind, AssetLoadState::Failed { error });
            }
        },
        IndexBuffer::ASSET_TYPE => match load_index_buffer(gpu, request.tag) {
            Ok(o) => {
                request.handle.update(o.into());
                manager.set_diagnostic(request.tag, kind, AssetLoadState::Ready);
            }
            Err(e) => {
                let error = format!("{e:#}");
                error!("Failed to load index buffer {}: {error}", request.tag);
                manager.set_diagnostic(request.tag, kind, AssetLoadState::Failed { error });
            }
        },
        Technique::ASSET_TYPE => {
            let result = match manager.era() {
                AssetEra::Current => Technique::load(gpu, manager, request.tag),
                AssetEra::Shadowkeep => Technique::load_shadowkeep(gpu, manager, request.tag),
            };
            match result {
                Ok(technique) => {
                    request.handle.update(technique.into());
                    manager.set_diagnostic(request.tag, kind, AssetLoadState::Ready);
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    error!("Failed to load {:?} technique {}: {error}", manager.era(), request.tag);
                    manager.set_diagnostic(request.tag, kind, AssetLoadState::Failed { error });
                }
            }
        }
        u => {
            panic!(
                "asset loader: Unknown asset type for tag {}: {u:?}",
                request.tag
            );
        }
    }

    num_loaded.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
}
