// ****************************************************************************************
// * THIS FILE IS BASED ON:
// *   https://github.com/containers/youki/blob/main/crates/youki/src/workload/wasmer.rs
// ****************************************************************************************
use std::process::exit;

use libcontainer::workload::{Executor, ExecutorError, EMPTY};
use log::debug;
use oci_spec::runtime::Spec;
use wasmer::{Module, Store};
use wasmer_wasix::{capabilities::Capabilities, WasiEnv};

const EXECUTOR_NAME: &str = "wasmer";

pub fn get_executor() -> Executor {
    log::info!("building {}", EXECUTOR_NAME);
    Box::new(|spec: &Spec| -> Result<(), ExecutorError> {
        log::info!("Can handle {}", EXECUTOR_NAME);
        //can_handle
        if let Some(annotations) = spec.annotations() {
            if let Some(handler) = annotations.get("youki.wasm.handler") {
                log::info!("Can handle {} == {}", handler.to_lowercase(), EXECUTOR_NAME);
                if handler.to_lowercase() != EXECUTOR_NAME {
                    return Err(ExecutorError::CantHandle(EXECUTOR_NAME));
                }
            }
        }

        log::info!("executing workload with {} handler", EXECUTOR_NAME);

        // parse wasi parameters
        let args = get_args(spec);
        let mut cmd = args[0].clone();
        if let Some(stripped) = args[0].strip_prefix(std::path::MAIN_SEPARATOR) {
            cmd = stripped.to_string();
        }

        let env = spec
            .process()
            .as_ref()
            .and_then(|p| p.env().as_ref())
            .unwrap_or(&EMPTY)
            .iter()
            .filter_map(|e| {
                e.split_once('=')
                    .filter(|kv| !kv.0.contains('\u{0}') && !kv.1.contains('\u{0}'))
                    .map(|kv| (kv.0.trim(), kv.1.trim()))
            });

        log::debug!("RUN {}: {} ({:?}) [{:?}]", EXECUTOR_NAME, cmd, args, env);
        debug!("RUN {}: {} ({:?}) [{:?}]", EXECUTOR_NAME, cmd, args, env);

        let mut store = Store::default();
        let module = Module::from_file(&store, cmd).unwrap();

        let _ = WasiEnv::builder("hello")
            .args(args)
            .envs(env)
            .capabilities(Capabilities {
                insecure_allow_all: true,
                http_client: Capabilities::default().http_client,
                threading: Capabilities::default().threading,
            })
            .run_with_store(module, &mut store);

        // shim for some reason hangs after execution
        // It solves the "entered unreachable code" the hard way
        exit(0);
        //Ok(())
    })
}

fn get_args(spec: &Spec) -> &[String] {
    let p = match spec.process() {
        None => return &[],
        Some(p) => p,
    };

    match p.args() {
        None => &[],
        Some(args) => args.as_slice(),
    }
}
