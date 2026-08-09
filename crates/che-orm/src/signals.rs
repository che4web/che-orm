use std::{
    any::TypeId,
    collections::HashMap,
    fmt,
    sync::{Arc, RwLock},
};

use crate::Model;
use serde_json::{Map, Value};
use tokio::sync::broadcast;

const SIGNAL_CHANNEL_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub struct PostSaveEvent {
    pub table: &'static str,
    pub created: bool,
    pub object: Value,
}

#[derive(Debug, Clone)]
pub struct PostUpdateEvent {
    pub table: &'static str,
    pub object: Value,
}

#[derive(Debug, Clone)]
pub enum ModelEvent {
    PostSave(PostSaveEvent),
    PostUpdate(PostUpdateEvent),
}

impl ModelEvent {
    pub fn table(&self) -> &'static str {
        match self {
            Self::PostSave(event) => event.table,
            Self::PostUpdate(event) => event.table,
        }
    }
}

#[derive(Clone)]
pub struct Signals {
    senders: Arc<RwLock<HashMap<TypeId, broadcast::Sender<ModelEvent>>>>,
}

impl fmt::Debug for Signals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Signals").finish_non_exhaustive()
    }
}

impl Signals {
    pub(crate) fn new() -> Self {
        Self {
            senders: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn subscribe<M: Model>(&self) -> broadcast::Receiver<ModelEvent> {
        self.sender::<M>().subscribe()
    }

    pub(crate) fn dispatch_post_save<M: Model>(&self, event: PostSaveEvent) {
        let _ = self.sender::<M>().send(ModelEvent::PostSave(event));
    }

    pub(crate) fn dispatch_post_update<M: Model>(&self, event: PostUpdateEvent) {
        let _ = self.sender::<M>().send(ModelEvent::PostUpdate(event));
    }

    fn sender<M: Model>(&self) -> broadcast::Sender<ModelEvent> {
        self.senders
            .write()
            .expect("signals registry lock is poisoned")
            .entry(TypeId::of::<M>())
            .or_insert_with(|| broadcast::channel(SIGNAL_CHANNEL_CAPACITY).0)
            .clone()
    }
}

pub(crate) fn snapshot<M: Model>(model: &M) -> Value {
    let values = M::fields().iter().filter_map(|field| {
        model
            .get_value(field.db_name)
            .map(|value| (field.rust_name.to_string(), value))
    });
    Value::Object(values.collect::<Map<_, _>>())
}
