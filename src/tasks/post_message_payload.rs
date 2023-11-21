use derivative::Derivative;
use js_sys::WebAssembly;
use wasm_bindgen::JsValue;
use wasmer_wasix::runtime::module_cache::ModuleHash;

use crate::tasks::{
    interop::Serializer, task_wasm::SpawnWasm, AsyncTask, BlockingModuleTask, BlockingTask,
};

/// A message that will be sent from the scheduler to a worker using
/// `postMessage()`.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) enum PostMessagePayload {
    SpawnAsync(#[derivative(Debug(format_with = "crate::utils::hidden"))] AsyncTask),
    SpawnBlocking(#[derivative(Debug(format_with = "crate::utils::hidden"))] BlockingTask),
    CacheModule {
        hash: ModuleHash,
        module: WebAssembly::Module,
    },
    SpawnWithModule {
        module: WebAssembly::Module,
        #[derivative(Debug(format_with = "crate::utils::hidden"))]
        task: BlockingModuleTask,
    },
    SpawnWithModuleAndMemory {
        module: WebAssembly::Module,
        /// An instance of the WebAssembly linear memory that has already been
        /// created.
        memory: Option<WebAssembly::Memory>,
        spawn_wasm: SpawnWasm,
    },
}

mod consts {
    pub(crate) const TYPE_SPAWN_ASYNC: &str = "spawn-async";
    pub(crate) const TYPE_SPAWN_BLOCKING: &str = "spawn-blocking";
    pub(crate) const TYPE_CACHE_MODULE: &str = "cache-module";
    pub(crate) const TYPE_SPAWN_WITH_MODULE: &str = "spawn-with-module";
    pub(crate) const TYPE_SPAWN_WITH_MODULE_AND_MEMORY: &str = "spawn-with-module-and-memory";
    pub(crate) const PTR: &str = "ptr";
    pub(crate) const MODULE: &str = "module";
    pub(crate) const MEMORY: &str = "memory";
    pub(crate) const MODULE_HASH: &str = "module-hash";
}

impl PostMessagePayload {
    #[tracing::instrument(level = "debug")]
    pub(crate) fn into_js(self) -> Result<JsValue, crate::utils::Error> {
        match self {
            PostMessagePayload::SpawnAsync(task) => Serializer::new(consts::TYPE_SPAWN_ASYNC)
                .boxed(consts::PTR, task)
                .finish(),
            PostMessagePayload::SpawnBlocking(task) => Serializer::new(consts::TYPE_SPAWN_BLOCKING)
                .boxed(consts::PTR, task)
                .finish(),
            PostMessagePayload::CacheModule { hash, module } => {
                Serializer::new(consts::TYPE_CACHE_MODULE)
                    .set(consts::MODULE_HASH, hash.to_string())
                    .set(consts::MODULE, module)
                    .finish()
            }
            PostMessagePayload::SpawnWithModule { module, task } => {
                Serializer::new(consts::TYPE_SPAWN_WITH_MODULE)
                    .boxed(consts::PTR, task)
                    .set(consts::MODULE, module)
                    .finish()
            }
            PostMessagePayload::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => Serializer::new(consts::TYPE_SPAWN_WITH_MODULE_AND_MEMORY)
                .boxed(consts::PTR, spawn_wasm)
                .set(consts::MODULE, module)
                .set(consts::MEMORY, memory)
                .finish(),
        }
    }

    /// Try to convert a [`PostMessagePayload`] back from a [`JsValue`].
    ///
    /// # Safety
    ///
    /// This can only be called if the original [`JsValue`] was created using
    /// [`PostMessagePayload::into_js()`].
    #[tracing::instrument(level = "debug")]
    pub(crate) unsafe fn try_from_js(value: JsValue) -> Result<Self, crate::utils::Error> {
        let de = crate::tasks::interop::Deserializer::new(value);

        // Safety: Keep this in sync with PostMessagePayload::to_js()
        match de.ty()?.as_str() {
            consts::TYPE_SPAWN_ASYNC => {
                let task = de.boxed(consts::PTR)?;
                Ok(PostMessagePayload::SpawnAsync(task))
            }
            consts::TYPE_SPAWN_BLOCKING => {
                let task = de.boxed(consts::PTR)?;
                Ok(PostMessagePayload::SpawnBlocking(task))
            }
            consts::TYPE_CACHE_MODULE => {
                let module = de.js(consts::MODULE)?;
                let hash = de.string(consts::MODULE_HASH)?;
                let hash = ModuleHash::parse_hex(&hash)?;

                Ok(PostMessagePayload::CacheModule { hash, module })
            }
            consts::TYPE_SPAWN_WITH_MODULE => {
                let task = de.boxed(consts::PTR)?;
                let module = de.js(consts::MODULE)?;

                Ok(PostMessagePayload::SpawnWithModule { module, task })
            }
            consts::TYPE_SPAWN_WITH_MODULE_AND_MEMORY => {
                let module = de.js(consts::MODULE)?;
                let memory = de.js(consts::MEMORY).ok();
                let spawn_wasm = de.boxed(consts::PTR)?;

                Ok(PostMessagePayload::SpawnWithModuleAndMemory {
                    module,
                    memory,
                    spawn_wasm,
                })
            }
            other => Err(anyhow::anyhow!("Unknown message type: {other}").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroUsize,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    use futures::channel::oneshot;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;
    use wasmer::AsJs;
    use wasmer_wasix::{
        runtime::{module_cache::ModuleHash, task_manager::TaskWasm},
        WasiEnvBuilder,
    };

    use crate::tasks::{SchedulerMessage, ThreadPool};

    use super::*;

    #[wasm_bindgen_test]
    async fn round_trip_spawn_blocking() {
        let flag = Arc::new(AtomicBool::new(false));
        let msg = PostMessagePayload::SpawnBlocking({
            let flag = Arc::clone(&flag);
            Box::new(move || {
                flag.store(true, Ordering::SeqCst);
            })
        });

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { PostMessagePayload::try_from_js(js).unwrap() };

        match round_tripped {
            PostMessagePayload::SpawnBlocking(task) => {
                task();
                assert!(flag.load(Ordering::SeqCst));
            }
            _ => unreachable!(),
        }
    }

    #[wasm_bindgen_test]
    async fn round_trip_spawn_async() {
        let flag = Arc::new(AtomicBool::new(false));
        let msg = PostMessagePayload::SpawnAsync({
            let flag = Arc::clone(&flag);
            Box::new(move || {
                Box::pin(async move {
                    flag.store(true, Ordering::SeqCst);
                })
            })
        });

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { PostMessagePayload::try_from_js(js).unwrap() };

        match round_tripped {
            PostMessagePayload::SpawnAsync(task) => {
                task().await;
                assert!(flag.load(Ordering::SeqCst));
            }
            _ => unreachable!(),
        }
    }

    #[wasm_bindgen_test]
    async fn round_trip_spawn_with_module() {
        let wasm: &[u8] = include_bytes!("../../tests/envvar.wasm");
        let engine = wasmer::Engine::default();
        let module = wasmer::Module::new(&engine, wasm).unwrap();
        let (sender, receiver) = oneshot::channel();
        let msg = PostMessagePayload::SpawnWithModule {
            module: JsValue::from(module).dyn_into().unwrap(),
            task: Box::new(|m| {
                sender
                    .send(
                        m.exports()
                            .map(|e| e.name().to_string())
                            .collect::<Vec<String>>(),
                    )
                    .unwrap();
            }),
        };

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { PostMessagePayload::try_from_js(js).unwrap() };

        let (module, task) = match round_tripped {
            PostMessagePayload::SpawnWithModule { module, task } => (module, task),
            _ => unreachable!(),
        };
        task(module.into());
        let name = receiver.await.unwrap();
        assert_eq!(
            name,
            vec![
                "memory".to_string(),
                "__heap_base".to_string(),
                "__data_end".to_string(),
                "_start".to_string(),
                "main".to_string()
            ]
        );
    }

    #[wasm_bindgen_test]
    async fn round_trip_cache_module() {
        let wasm: &[u8] = include_bytes!("../../tests/envvar.wasm");
        let engine = wasmer::Engine::default();
        let module = wasmer::Module::new(&engine, wasm).unwrap();
        let msg = PostMessagePayload::CacheModule {
            hash: ModuleHash::sha256(wasm),
            module: module.into(),
        };

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { PostMessagePayload::try_from_js(js).unwrap() };

        match round_tripped {
            PostMessagePayload::CacheModule { hash, module: _ } => {
                assert_eq!(hash, ModuleHash::sha256(wasm));
            }
            _ => unreachable!(),
        };
    }

    #[wasm_bindgen_test]
    async fn round_trip_spawn_with_module_and_memory() {
        let wasm: &[u8] = include_bytes!("../../tests/envvar.wasm");
        let engine = wasmer::Engine::default();
        let module = wasmer::Module::new(&engine, wasm).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let pool = ThreadPool::new(NonZeroUsize::MAX);
        let runtime = crate::Runtime::new(pool);
        let env = WasiEnvBuilder::new("program")
            .runtime(Arc::new(runtime))
            .build()
            .unwrap();
        let msg = crate::tasks::task_wasm::to_scheduler_message(TaskWasm::new(
            Box::new({
                let flag = Arc::clone(&flag);
                move |_| {
                    flag.store(true, Ordering::SeqCst);
                }
            }),
            env,
            module,
            false,
        ))
        .unwrap();
        let msg = match msg {
            SchedulerMessage::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => PostMessagePayload::SpawnWithModuleAndMemory {
                module: module.into(),
                memory: memory.map(|m| m.as_jsvalue(&wasmer::Store::default()).dyn_into().unwrap()),
                spawn_wasm,
            },
            _ => unreachable!(),
        };

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { PostMessagePayload::try_from_js(js).unwrap() };

        let (module, memory, spawn_wasm) = match round_tripped {
            PostMessagePayload::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => (module, memory, spawn_wasm),
            _ => unreachable!(),
        };
        spawn_wasm
            .begin()
            .await
            .execute(module, memory.into())
            .unwrap();
        assert!(flag.load(Ordering::SeqCst));
    }
}