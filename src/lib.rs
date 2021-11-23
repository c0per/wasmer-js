use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasmer::{
    ChainableNamedResolver, ImportObject, Instance, JSObjectResolver, Module, NamedResolverChain,
};
use wasmer_wasi::{Stdin, Stdout, WasiEnv, WasiError, WasiState};

struct InstantiatedWASI {
    instance: Instance,
    #[allow(dead_code)]
    resolver: NamedResolverChain<ImportObject, JSObjectResolver>,
}

#[wasm_bindgen]
pub struct WASI {
    wasi_env: WasiEnv,
    instantiated: Option<InstantiatedWASI>,
}

#[wasm_bindgen]
impl WASI {
    #[wasm_bindgen(constructor)]
    pub fn new(config: JsValue) -> Result<WASI, JsValue> {
        let args: Vec<String> = {
            let args = js_sys::Reflect::get(&config, &"args".into())?;
            if args.is_undefined() {
                vec![]
            } else {
                let args_array: js_sys::Array = args.dyn_into()?;
                args_array
                    .iter()
                    .map(|arg| {
                        arg.as_string()
                            .ok_or(js_sys::Error::new("All arguments must be strings").into())
                    })
                    .collect::<Result<Vec<String>, JsValue>>()?
            }
        };
        let env: Vec<(String, String)> = {
            let env = js_sys::Reflect::get(&config, &"env".into())?;
            if env.is_undefined() {
                vec![]
            } else {
                let env_obj: js_sys::Object = env.dyn_into()?;
                js_sys::Object::entries(&env_obj)
                    .iter()
                    .map(|entry| {
                        let entry: js_sys::Array = entry.unchecked_into();
                        let key: Result<String, JsValue> = entry.get(0).as_string().ok_or(
                            js_sys::Error::new("All environment keys must be strings").into(),
                        );
                        let value: Result<String, JsValue> = entry.get(1).as_string().ok_or(
                            js_sys::Error::new("All environment values must be strings").into(),
                        );
                        key.and_then(|key| Ok((key, value?)))
                    })
                    .collect::<Result<Vec<(String, String)>, JsValue>>()?
            }
        };

        let wasi_env = WasiState::new(&args.get(0).unwrap_or(&"".to_string()))
            .args(if args.len() > 0 { &args[1..] } else { &[] })
            .envs(env)
            .finalize()
            .map_err(|e| js_sys::Error::new(&format!("Failed to create the WasiState: {}`", e)))?;

        Ok(WASI {
            wasi_env,
            instantiated: None,
        })
    }

    pub fn instantiate(&mut self, module: JsValue, imports: js_sys::Object) -> Result<(), JsValue> {
        let module: js_sys::WebAssembly::Module = module.dyn_into().map_err(|_e| {
            js_sys::Error::new(
                "You must provide a module to the WASI new. `let module = new WASI({}, module);`",
            )
        })?;
        let module: Module = module.into();
        let import_object = self.wasi_env.import_object(&module).map_err(|e| {
            js_sys::Error::new(&format!("Failed to create the Import Object: {}`", e))
        })?;

        let resolver = JSObjectResolver::new(&module, imports);
        let resolver = resolver.chain_front(import_object);

        let instance = Instance::new(&module, &resolver)
            .map_err(|e| js_sys::Error::new(&format!("Failed to instantiate WASI: {}`", e)))?;
        self.instantiated = Some(InstantiatedWASI { resolver, instance });
        Ok(())
    }

    /// Start the WASI Instance, it returns the status code when calling the start
    /// function
    pub fn start(&self) -> Result<u32, JsValue> {
        let start = self
            .instantiated
            .as_ref()
            .unwrap()
            .instance
            .exports
            .get_function("_start")
            .map_err(|_e| js_sys::Error::new("The _start function is not present"))?;
        let result = start.call(&[]);

        match result {
            Ok(_) => Ok(0),
            Err(err) => {
                match err.downcast::<WasiError>() {
                    Ok(WasiError::Exit(exit_code)) => {
                        // We should exit with the provided exit code
                        return Ok(exit_code);
                    }
                    Ok(err) => {
                        return Err(js_sys::Error::new(&format!(
                            "Unexpected WASI error while running start function: {}",
                            err
                        ))
                        .into())
                    }
                    Err(err) => {
                        return Err(js_sys::Error::new(&format!(
                            "Error while running start function: {}",
                            err
                        ))
                        .into())
                    }
                }
            }
        }
    }

    #[wasm_bindgen(js_name = getStdoutBuffer)]
    pub fn get_stdout_buffer(&self) -> Vec<u8> {
        let state = self.wasi_env.state();
        let stdout = state.fs.stdout().unwrap().as_ref().unwrap();
        let stdout = stdout.downcast_ref::<Stdout>().unwrap();
        stdout.buf.clone()
    }

    #[wasm_bindgen(js_name = getStdoutString)]
    pub fn get_stdout_string(&self) -> String {
        String::from_utf8(self.get_stdout_buffer()).unwrap()
    }
}