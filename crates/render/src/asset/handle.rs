use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use parking_lot::RwLock;
use tiger_pkg::TagHash;

use super::Asset;
use crate::tfx::technique::Technique;

enum AssetState {
    Loading,
    Ready(Arc<dyn Any + Send + Sync>),
    Failed(Arc<str>),
}

struct AssetHolder {
    state: RwLock<AssetState>,
    ref_count: AtomicUsize,
}

impl AssetHolder {
    fn new() -> Self {
        Self {
            state: RwLock::new(AssetState::Loading),
            ref_count: AtomicUsize::new(1),
        }
    }
}

pub struct UntypedHandle {
    inner: Arc<AssetHolder>,
    pub tag: TagHash,
}

impl Default for UntypedHandle {
    fn default() -> Self {
        Self::new(TagHash::NONE)
    }
}

impl UntypedHandle {
    pub fn new(tag: TagHash) -> Self {
        Self {
            inner: Arc::new(AssetHolder::new()),
            tag,
        }
    }

    pub fn is_loaded(&self) -> bool {
        !matches!(*self.inner.state.read(), AssetState::Loading)
    }

    /// # Safety
    /// The caller must ensure that the asset is of the correct type.
    pub unsafe fn clone_as_typed_unchecked<T: Asset>(&self) -> Handle<T> {
        Handle {
            asset: self.clone(),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn update<T: Asset + Send + Sync + 'static>(&self, asset: Box<T>) {
        let mut state = self.inner.state.write();
        if !matches!(*state, AssetState::Loading) {
            error!(
                "Attempted to complete terminal asset handle {} more than once",
                self.tag
            );
            return;
        }

        *state = AssetState::Ready(Arc::<T>::from(asset));
    }

    pub fn fail(&self, error: impl Into<Arc<str>>) {
        let mut state = self.inner.state.write();
        if !matches!(*state, AssetState::Loading) {
            error!(
                "Attempted to complete terminal asset handle {} more than once",
                self.tag
            );
            return;
        }

        *state = AssetState::Failed(error.into());
    }

    pub fn failure(&self) -> Option<Arc<str>> {
        match &*self.inner.state.read() {
            AssetState::Failed(error) => Some(error.clone()),
            AssetState::Loading | AssetState::Ready(_) => None,
        }
    }

    pub fn ref_count(&self) -> usize {
        self.inner.ref_count.load(Ordering::Relaxed)
    }
}

impl Clone for UntypedHandle {
    fn clone(&self) -> Self {
        self.inner.ref_count.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
            tag: self.tag,
        }
    }
}

impl Drop for UntypedHandle {
    fn drop(&mut self) {
        self.inner.ref_count.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct Handle<T: Asset + 'static> {
    asset: UntypedHandle,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Asset + Sync + Send + 'static> Handle<T> {
    /// Returns true if the handle is null or the asset is loaded
    pub fn is_loaded(&self) -> bool {
        self.is_null() || self.asset.is_loaded()
    }

    pub fn is_null(&self) -> bool {
        self.asset.tag.is_none()
    }

    pub fn hash(&self) -> TagHash {
        self.asset.tag
    }

    pub fn get(&self) -> Option<Arc<T>> {
        let state = self.asset.inner.state.read();
        let AssetState::Ready(data) = &*state else {
            return None;
        };
        Arc::downcast(Arc::clone(data)).ok()
    }

    // Passes the ref in a closure to avoid cloning the Arc unnecessarily
    pub fn get_ref<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&T) -> R,
    {
        let state = self.asset.inner.state.read();
        let AssetState::Ready(data) = &*state else {
            return None;
        };
        Some(f(data.downcast_ref::<T>()?))
    }

    pub fn update(&self, asset: Box<T>) {
        self.asset.update(asset);
    }

    pub fn failure(&self) -> Option<Arc<str>> {
        self.asset.failure()
    }

    pub fn ref_count(&self) -> usize {
        self.asset.ref_count()
    }
}

impl<T: Asset + 'static> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self {
            asset: self.asset.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: Asset + 'static> Default for Handle<T> {
    fn default() -> Self {
        Self {
            asset: UntypedHandle::default(),
            _marker: std::marker::PhantomData,
        }
    }
}

pub fn is_technique_loaded(handle: &Handle<Technique>) -> bool {
    if handle.is_null() {
        return true;
    }

    let Some(technique) = handle.get() else {
        return false;
    };

    technique.is_loaded()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{Asset, Handle, UntypedHandle};

    struct TestAsset(u32);

    impl Asset for TestAsset {
        const ASSET_TYPE: Uuid = Uuid::nil();
    }

    fn typed(asset: &UntypedHandle) -> Handle<TestAsset> {
        Handle {
            asset: asset.clone(),
            _marker: std::marker::PhantomData,
        }
    }

    #[test]
    fn successful_publication_is_terminal_and_typed() {
        let asset = UntypedHandle::default();
        asset.update(Box::new(TestAsset(42)));

        assert!(asset.is_loaded());
        assert_eq!(typed(&asset).get().unwrap().0, 42);
        assert!(asset.failure().is_none());
    }

    #[test]
    fn failed_publication_is_terminal_without_fake_data() {
        let asset = UntypedHandle::default();
        asset.fail("decode failed");
        asset.update(Box::new(TestAsset(42)));

        assert!(asset.is_loaded());
        assert!(typed(&asset).get().is_none());
        assert_eq!(asset.failure().as_deref(), Some("decode failed"));
    }
}
