use std::{
    any::TypeId,
    collections::HashMap,
    fmt,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex, RwLock},
};

use crate::Model;
use futures_util::FutureExt;
use serde_json::{Map, Value};
use tokio::{sync::mpsc, task::JoinHandle};

const SIGNAL_CHANNEL_CAPACITY: usize = 1024;

pub type SignalError = Box<dyn std::error::Error + Send + Sync>;

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

#[async_trait::async_trait]
pub trait PostSaveHandler: Send + Sync + 'static {
    async fn handle(&self, event: PostSaveEvent) -> Result<(), SignalError>;
}

#[async_trait::async_trait]
pub trait PostUpdateHandler: Send + Sync + 'static {
    async fn handle(&self, event: PostUpdateEvent) -> Result<(), SignalError>;
}

#[derive(Default)]
struct Handlers {
    post_save: HashMap<TypeId, Vec<Arc<dyn PostSaveHandler>>>,
    post_update: HashMap<TypeId, Vec<Arc<dyn PostUpdateHandler>>>,
}

#[derive(Clone)]
enum SignalEvent {
    PostSave {
        model: TypeId,
        event: PostSaveEvent,
    },
    PostUpdate {
        model: TypeId,
        event: PostUpdateEvent,
    },
}

impl SignalEvent {
    fn table(&self) -> &'static str {
        match self {
            Self::PostSave { event, .. } => event.table,
            Self::PostUpdate { event, .. } => event.table,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::PostSave { .. } => "post_save",
            Self::PostUpdate { .. } => "post_update",
        }
    }
}

#[derive(Clone)]
pub struct Signals {
    handlers: Arc<RwLock<Handlers>>,
    sender: mpsc::Sender<SignalEvent>,
    _dispatcher: Arc<Dispatcher>,
}

struct Dispatcher {
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        if let Some(handle) = self
            .handle
            .get_mut()
            .expect("signal dispatcher lock is poisoned")
            .take()
        {
            handle.abort();
        }
    }
}

impl fmt::Debug for Signals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Signals").finish_non_exhaustive()
    }
}

impl Signals {
    pub(crate) fn new() -> Self {
        let handlers = Arc::new(RwLock::new(Handlers::default()));
        let (sender, receiver) = mpsc::channel(SIGNAL_CHANNEL_CAPACITY);
        let dispatcher = Arc::new(Dispatcher {
            handle: Mutex::new(None),
        });
        let handle = spawn_dispatcher(receiver, handlers.clone());
        *dispatcher
            .handle
            .lock()
            .expect("signal dispatcher lock is poisoned") = Some(handle);
        Self {
            handlers,
            sender,
            _dispatcher: dispatcher,
        }
    }

    pub fn post_save<M>(&self, handler: impl PostSaveHandler)
    where
        M: Model,
    {
        self.handlers
            .write()
            .expect("signals registry lock is poisoned")
            .post_save
            .entry(TypeId::of::<M>())
            .or_default()
            .push(Arc::new(handler));
    }

    pub fn post_save_for<M>(&self, handler: impl PostSaveHandler)
    where
        M: Model,
    {
        self.post_save::<M>(handler);
    }

    pub fn post_update<M>(&self, handler: impl PostUpdateHandler)
    where
        M: Model,
    {
        self.handlers
            .write()
            .expect("signals registry lock is poisoned")
            .post_update
            .entry(TypeId::of::<M>())
            .or_default()
            .push(Arc::new(handler));
    }

    pub fn post_update_for<M>(&self, handler: impl PostUpdateHandler)
    where
        M: Model,
    {
        self.post_update::<M>(handler);
    }

    pub(crate) fn dispatch_post_save<M: Model>(&self, event: PostSaveEvent) {
        self.dispatch(SignalEvent::PostSave {
            model: TypeId::of::<M>(),
            event,
        });
    }

    pub(crate) fn dispatch_post_update<M: Model>(&self, event: PostUpdateEvent) {
        self.dispatch(SignalEvent::PostUpdate {
            model: TypeId::of::<M>(),
            event,
        });
    }

    fn dispatch(&self, event: SignalEvent) {
        if let Err(error) = self.sender.try_send(event) {
            let event = error.into_inner();
            tracing::warn!(
                table = event.table(),
                signal = event.name(),
                "signal queue is full or unavailable; dropping event"
            );
        }
    }
}

fn spawn_dispatcher(
    mut receiver: mpsc::Receiver<SignalEvent>,
    handlers: Arc<RwLock<Handlers>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            match event {
                SignalEvent::PostSave { model, event } => {
                    let handlers = handlers
                        .read()
                        .expect("signals registry lock is poisoned")
                        .post_save
                        .get(&model)
                        .cloned()
                        .unwrap_or_default();
                    for handler in handlers {
                        dispatch_post_save_handler(handler, event.clone()).await;
                    }
                }
                SignalEvent::PostUpdate { model, event } => {
                    let handlers = handlers
                        .read()
                        .expect("signals registry lock is poisoned")
                        .post_update
                        .get(&model)
                        .cloned()
                        .unwrap_or_default();
                    for handler in handlers {
                        dispatch_post_update_handler(handler, event.clone()).await;
                    }
                }
            }
        }
    })
}

async fn dispatch_post_save_handler(handler: Arc<dyn PostSaveHandler>, event: PostSaveEvent) {
    match AssertUnwindSafe(handler.handle(event)).catch_unwind().await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(error = %error, "post_save handler failed"),
        Err(_) => tracing::error!("post_save handler panicked"),
    }
}

async fn dispatch_post_update_handler(handler: Arc<dyn PostUpdateHandler>, event: PostUpdateEvent) {
    match AssertUnwindSafe(handler.handle(event)).catch_unwind().await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(error = %error, "post_update handler failed"),
        Err(_) => tracing::error!("post_update handler panicked"),
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
