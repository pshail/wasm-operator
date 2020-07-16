use crate::{
    api::{Api, ListParams, Meta, WatchEvent},
    Error, Result,
};
use serde::de::DeserializeOwned;

use std::{collections::BTreeMap, sync::Arc, sync::Mutex};

/// A reflection of state for a Kubernetes ['Api'] resource
///
/// This builds on top of the ['Informer'] by tracking the events received,
/// via ['Informer::poll']. This object will in fact use .poll() continuously,
/// and use the results to maintain an up to date state map.
///
/// It is prone to the same desync problems as an informer, but it will self-heal,
/// as best as possible - though this means that you might occasionally see a full
/// reset (boot equivalent) when network issues are encountered.
/// During a reset, the state is cleared before it is rebuilt.
///
/// The internal state is exposed readably through a getter.
#[derive(Clone)]
pub struct Reflector<K>
where
    K: Clone + DeserializeOwned + Meta,
{
    state: Arc<Mutex<State<K>>>,
    params: ListParams,
    api: Api<K>,
}

impl<K> Reflector<K>
where
    K: Clone + DeserializeOwned + Meta,
{
    /// Create a reflector on an api resource
    pub fn new(api: Api<K>) -> Self {
        Reflector {
            api,
            params: ListParams::default(),
            state: Default::default(),
        }
    }

    /// Modify the default watch parameters for the underlying watch
    pub fn params(mut self, lp: ListParams) -> Self {
        self.params = lp;
        self
    }

    /// A single poll call to modify the internal state
    pub fn poll(&self) -> Result<()> {
        let kind = &self.api.resource.kind;
        let resource_version = self.state.lock().unwrap().version.clone();
        trace!("Polling {} from resourceVersion={}", kind, resource_version);
        let events = self.api.watch(&self.params, &resource_version)?;

        for ev in events {
            let mut state = self.state.lock().unwrap();
            // Informer-like version tracking:
            match &ev {
                Ok(WatchEvent::Added(o))
                | Ok(WatchEvent::Modified(o))
                | Ok(WatchEvent::Deleted(o))
                | Ok(WatchEvent::Bookmark(o)) => {
                    // always store the last seen resourceVersion
                    if let Some(nv) = Meta::resource_ver(o) {
                        trace!("Updating reflector version for {} to {}", kind, nv);
                        state.version = nv.clone();
                    }
                }
                _ => {}
            }

            let data = &mut state.data;
            // Core Reflector logic
            match ev {
                Ok(WatchEvent::Added(o)) => {
                    debug!("Adding {} to {}", Meta::name(&o), kind);
                    data.entry(ObjectId::key_for(&o))
                        .or_insert_with(|| o.clone());
                }
                Ok(WatchEvent::Modified(o)) => {
                    debug!("Modifying {} in {}", Meta::name(&o), kind);
                    data.entry(ObjectId::key_for(&o))
                        .and_modify(|e| *e = o.clone());
                }
                Ok(WatchEvent::Deleted(o)) => {
                    debug!("Removing {} from {}", Meta::name(&o), kind);
                    data.remove(&ObjectId::key_for(&o));
                }
                Ok(WatchEvent::Bookmark(o)) => {
                    debug!("Bookmarking {} from {}", Meta::name(&o), kind);
                }
                Ok(WatchEvent::Error(e)) => {
                    warn!("Failed to watch {}: {:?}", kind, e);
                    return Err(Error::Api(e));
                }
                Err(e) => {
                    warn!("Received error while watcing {}: {:?}", kind, e);
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Reset the state of the underlying informer and clear the cache
    pub fn reset(&self) -> Result<()> {
        trace!("Resetting {}", self.api.resource.kind);
        // Simplified for k8s >= 1.16
        //*self.state.lock().await = Default::default();
        //self.informer.reset().await

        // For now:
        let (data, version) = self.get_full_resource_entries()?;
        *self.state.lock().unwrap() = State { data, version };
        Ok(())
    }

    /// Legacy helper for kubernetes < 1.16
    ///
    /// Needed to do an initial list operation because of https://github.com/clux/kube-rs/issues/219
    /// Soon, this goes away as we drop support for k8s < 1.16
    fn get_full_resource_entries(&self) -> Result<(Cache<K>, String)> {
        let res = self.api.list(&self.params)?;
        let version = res.metadata.resource_version.unwrap_or_default();
        trace!(
            "Got {} {} at resourceVersion={:?}",
            res.items.len(),
            self.api.resource.kind,
            version
        );
        let mut data = BTreeMap::new();
        for i in res.items {
            // The non-generic parts we care about are spec + status
            data.insert(ObjectId::key_for(&i), i);
        }
        let keys = data
            .keys()
            .map(ObjectId::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        debug!("Initialized with: [{}]", keys);
        Ok((data, version))
    }

    /// Read data for users of the reflector
    ///
    /// This is instant if you are reading and writing from the same context.
    pub fn state(&self) -> Result<Vec<K>> {
        let state = self.state.lock().unwrap();
        Ok(state.data.values().cloned().collect::<Vec<K>>())
    }

    /// Read a single entry by name
    ///
    /// Will read in the configured namespace, or globally on non-namespaced reflectors.
    /// If you are using a non-namespaced resources with name clashes,
    /// Try [`Reflector::get_within`] instead.
    pub fn get(&self, name: &str) -> Result<Option<K>> {
        let id = ObjectId {
            name: name.into(),
            namespace: self.api.resource.namespace.clone(),
        };

        Ok(self.state.lock().unwrap().data.get(&id).map(Clone::clone))
    }

    /// Read a single entry by name within a specific namespace
    ///
    /// This is a more specific version of [`Reflector::get`].
    /// This is only useful if your reflector is configured to poll across namespaces.
    /// TODO: remove once #194 is resolved
    pub fn get_within(&self, name: &str, ns: &str) -> Result<Option<K>> {
        let id = ObjectId {
            name: name.into(),
            namespace: Some(ns.into()),
        };
        Ok(self.state.lock().unwrap().data.get(&id).map(Clone::clone))
    }
}

/// ObjectId represents an object by name and namespace (if any)
///
/// This is an internal subset of ['k8s_openapi::api::core::v1::ObjectReference']
#[derive(Ord, PartialOrd, Hash, Eq, PartialEq, Clone)]
struct ObjectId {
    name: String,
    namespace: Option<String>,
}

impl ToString for ObjectId {
    fn to_string(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{} [{}]", self.name, ns),
            None => self.name.clone(),
        }
    }
}

impl ObjectId {
    fn key_for<K: Meta>(o: &K) -> Self {
        ObjectId {
            name: Meta::name(o),
            namespace: Meta::namespace(o),
        }
    }
}

/// Internal shared state of Reflector
///
/// Can remove this in k8s >= 1.16 once this uses Informer
struct State<K> {
    data: Cache<K>,
    version: String,
}

impl<K> Default for State<K> {
    fn default() -> Self {
        State {
            data: Default::default(),
            version: 0.to_string(),
        }
    }
}
/// Internal representation for Reflector
type Cache<K> = BTreeMap<ObjectId, K>;
