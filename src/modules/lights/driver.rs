//! The interface between the module and the devices.
//!
//! [`LightDriver`] is implemented per device family ([`super::tuya`] today),
//! so that supporting another kind of bulb (or swapping the Tuya library)
//! does not touch the module. [`DriverPool`] keeps one driver per configured
//! light and rebuilds it when its configuration changes.

use std::{collections::HashMap, sync::Arc};

use futures::{future::BoxFuture, stream::BoxStream};
use tokio::sync::Mutex;

use super::{
    model::{LightChange, LightState},
    settings::DeviceConfig,
};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LightError {
    #[error("couldn't reach the light: {0}")]
    Unreachable(String),

    #[error("the light gave an unexpected answer: {0}")]
    Protocol(String),

    #[error("{0}")]
    Unsupported(String),
}

pub type LightResult<T> = Result<T, LightError>;

pub trait LightDriver: Send + Sync + 'static {
    fn state(&self) -> BoxFuture<'_, LightResult<LightState>>;

    /// Applies the change and returns the resulting state.
    fn apply(&self, change: LightChange) -> BoxFuture<'_, LightResult<LightState>>;

    /// The states the light reports on its own, e.g. when it is changed from
    /// another app. Ends when the driver is dropped.
    fn watch(&self) -> BoxStream<'static, LightState> {
        Box::pin(futures::stream::empty())
    }
}

/// Builds the driver of a configured light.
pub type DriverFactory = Arc<dyn Fn(&DeviceConfig) -> Arc<dyn LightDriver> + Send + Sync>;

/// A driver along with the configuration it was built from.
type CachedDriver = (DeviceConfig, Arc<dyn LightDriver>);

/// One driver per light, created on first use.
pub struct DriverPool {
    factory: DriverFactory,
    drivers: Mutex<HashMap<String, CachedDriver>>,
}

impl DriverPool {
    pub fn new(factory: DriverFactory) -> Self {
        Self {
            factory,
            drivers: Mutex::new(HashMap::new()),
        }
    }

    /// The driver of the light `name`, (re)built if its configuration changed
    /// since the last call (e.g. after `/config reload`).
    pub async fn get(&self, name: &str, config: &DeviceConfig) -> Arc<dyn LightDriver> {
        let mut drivers = self.drivers.lock().await;
        match drivers.get(name) {
            Some((cached, driver)) if cached == config => Arc::clone(driver),
            _ => {
                tracing::debug!(light = name, "creating the light's driver");
                let driver = (self.factory)(config);
                drivers.insert(name.to_string(), (config.clone(), Arc::clone(&driver)));
                driver
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! An in-memory light, for tests.

    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::modules::lights::model::Mode;

    fn tokio_stream_from(
        receiver: tokio::sync::broadcast::Receiver<LightState>,
    ) -> impl futures::Stream<Item = LightState> + Send + 'static {
        futures::stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(state) => return Some((state, receiver)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
    }

    pub struct FakeLight {
        pub state: StdMutex<LightResult<LightState>>,
        pub changes: StdMutex<Vec<LightChange>>,
        /// Pushes a state to the watchers, as if changed from another app.
        pub pushes: tokio::sync::broadcast::Sender<LightState>,
    }

    impl FakeLight {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                state: StdMutex::new(Ok(LightState {
                    on: false,
                    mode: Mode::White,
                    brightness: 50,
                    temperature: Some(50),
                    color: None,
                    scene: None,
                    supports_color: true,
                })),
                changes: StdMutex::new(Vec::new()),
                pushes: tokio::sync::broadcast::channel(16).0,
            })
        }

        pub fn unreachable() -> Arc<Self> {
            let light = Self::new();
            *light.state.lock().unwrap() = Err(LightError::Unreachable("timeout".into()));
            light
        }

        pub fn current(&self) -> LightState {
            self.state.lock().unwrap().clone().unwrap()
        }

        /// Changes the light behind the bot's back.
        pub fn change_externally(&self, change: impl FnOnce(&mut LightState)) {
            let mut state = self.state.lock().unwrap();
            let state = state.as_mut().unwrap();
            change(state);
            let _ = self.pushes.send(state.clone());
        }
    }

    impl LightDriver for FakeLight {
        fn state(&self) -> BoxFuture<'_, LightResult<LightState>> {
            Box::pin(async move { self.state.lock().unwrap().clone() })
        }

        fn apply(&self, change: LightChange) -> BoxFuture<'_, LightResult<LightState>> {
            Box::pin(async move {
                self.changes.lock().unwrap().push(change.clone());
                let mut state = self.state.lock().unwrap();
                let current = state.as_mut().map_err(|error| error.clone())?;
                current.on = change.on.unwrap_or(true);
                if let Some(brightness) = change.brightness {
                    current.brightness = brightness;
                    if change.scene.is_none()
                        && let Some(scene) = &current.scene
                    {
                        current.scene = Some(scene.with_brightness(brightness));
                    }
                }
                if let Some(temperature) = change.temperature {
                    current.mode = Mode::White;
                    current.temperature = Some(temperature);
                    current.color = None;
                }
                if let Some(color) = change.color {
                    current.mode = Mode::Colour;
                    current.color = Some(color);
                    current.temperature = None;
                }
                if let Some(scene) = change.scene {
                    current.mode = Mode::Scene;
                    current.brightness = change.brightness.unwrap_or(scene.brightness());
                    current.scene = Some(scene);
                    current.temperature = None;
                    current.color = None;
                }
                Ok(current.clone())
            })
        }

        fn watch(&self) -> BoxStream<'static, LightState> {
            let pushes = tokio_stream_from(self.pushes.subscribe());
            Box::pin(pushes)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{fake::FakeLight, *};
    use crate::config::Secret;

    fn config(address: &str) -> DeviceConfig {
        DeviceConfig {
            id: "id".into(),
            local_key: Secret::new("key".into()),
            address: Some(address.into()),
            version: None,
            layout: Default::default(),
        }
    }

    #[tokio::test]
    async fn drivers_are_reused_until_their_config_changes() {
        let built = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&built);
        let pool = DriverPool::new(Arc::new(move |_: &DeviceConfig| {
            counter.fetch_add(1, Ordering::SeqCst);
            FakeLight::new() as Arc<dyn LightDriver>
        }));

        pool.get("desk", &config("10.0.0.1")).await;
        pool.get("desk", &config("10.0.0.1")).await;
        assert_eq!(built.load(Ordering::SeqCst), 1);

        pool.get("desk", &config("10.0.0.2")).await;
        pool.get("lamp", &config("10.0.0.2")).await;
        assert_eq!(built.load(Ordering::SeqCst), 3);
    }
}
