use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::thread;
use std::{
    fs::{self, File},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

use chrono::{DateTime, Utc};
use containerd_shim as shim;
use containerd_shim_wasm::sandbox::{
    instance::{InstanceConfig, Wait},
    EngineGetter, Error, Instance, ShimCli,
};
use libc::{SIGINT, SIGKILL};
use libcontainer::{
    container::builder::ContainerBuilder, syscall::syscall::create_syscall,
    workload::default::DefaultExecutor,
};
use log::error;
use nix::errno::Errno;
use nix::sys::wait::{waitid, Id as WaitID, WaitPidFlag, WaitStatus};

use libcontainer::container::{Container, ContainerStatus};
use libcontainer::signal::Signal;

type ExitCode = Arc<(Mutex<Option<(u32, DateTime<Utc>)>>, Condvar)>;
static DEFAULT_CONTAINER_ROOT_DIR: &str = " /run/containerd/youki";

pub struct MyContainer {
    exit_code: ExitCode,
    id: String,
    stdin: String,
    stdout: String,
    stderr: String,
    bundle: String,
    shutdown_signal: Arc<(Mutex<bool>, Condvar)>,

    rootdir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Options {
    root: Option<PathBuf>,
}

fn determine_rootdir<P: AsRef<Path>>(bundle: P, namespace: String) -> Result<PathBuf, Error> {
    let mut file = match File::open(bundle.as_ref().join("options.json")) {
        Ok(f) => f,
        Err(err) => match err.kind() {
            ErrorKind::NotFound => {
                return Ok(<&str as Into<PathBuf>>::into(DEFAULT_CONTAINER_ROOT_DIR).join(namespace))
            }
            _ => return Err(err.into()),
        },
    };
    let mut data = String::new();
    file.read_to_string(&mut data)?;
    let options: Options = serde_json::from_str(&data)?;
    Ok(options
        .root
        .unwrap_or(PathBuf::from(DEFAULT_CONTAINER_ROOT_DIR))
        .join(namespace))
}

impl Instance for MyContainer {
    type E = ();

    fn new(id: String, cfg: Option<&InstanceConfig<Self::E>>) -> Self {
        log::info!(">>> New instance: {}", id);
        let cfg = cfg.unwrap(); // TODO: handle error
        let bundle = cfg.get_bundle().unwrap_or_default();
        let namespace = cfg.get_namespace();
        MyContainer {
            id,
            exit_code: Arc::new((Mutex::new(None), Condvar::new())),
            stdin: cfg.get_stdin().unwrap_or_default(),
            stdout: cfg.get_stdout().unwrap_or_default(),
            stderr: cfg.get_stderr().unwrap_or_default(),
            bundle: bundle.clone(),
            shutdown_signal: Arc::new((Mutex::new(false), Condvar::new())),
            rootdir: determine_rootdir(bundle.as_str(), namespace).unwrap(),
        }
    }

    fn start(&self) -> Result<u32, containerd_shim_wasm::sandbox::Error> {
        log::info!(">>> Starting container {}", self.id);

        log::info!(">>> About to build DefaultContainer {}", self.id);
        let mut container = match self.build_executor() {
            Ok(c) => c,
            Err(err) => {
                error!("failed to build container: {}", err);
                return Err(Error::Others(err.to_string()));
            }
        };
        log::info!(">>> Built DefaultContainer {}", self.id);
        let code = self.exit_code.clone();
        log::info!(">>> About to run container {}", self.id);
        let pid = container.pid().unwrap();
        match container.start() {
            Ok(_) => {}
            Err(err) => {
                error!("failed to start container: {}", err);
                return Err(Error::Others(err.to_string()));
            }
        }
        log::info!(">>> Running container pid: {}", pid);
        thread::spawn(move || {
            let (lock, cvar) = &*code;
            let status = match waitid(WaitID::Pid(pid), WaitPidFlag::WEXITED) {
                Ok(WaitStatus::Exited(_, status)) => status,
                Ok(WaitStatus::Signaled(_, sig, _)) => sig as i32,
                Ok(_) => 0,
                Err(e) => {
                    if e == Errno::ECHILD {
                        0
                    } else {
                        panic!("waitpid failed: {}", e);
                    }
                }
            } as u32;
            let mut ec = lock.lock().unwrap();
            *ec = Some((status, Utc::now()));
            drop(ec);
            cvar.notify_all();
        });
        Ok(pid.as_raw() as u32)
    }

    fn kill(&self, signal: u32) -> Result<(), containerd_shim_wasm::sandbox::Error> {
        log::info!(">>> Killing container {}", self.id);
        if signal as i32 != SIGKILL && signal as i32 != SIGINT {
            return Err(Error::InvalidArgument(
                "only SIGKILL and SIGINT are supported".to_string(),
            ));
        }

        let mut container = load_container(&self.rootdir, self.id.as_str())?;
        match container.kill(Signal::try_from(signal as i32).unwrap(), true) {
            Ok(_) => Ok(()),
            Err(e) => {
                if container.status() == ContainerStatus::Stopped {
                    return Err(Error::Others("container not running".into()));
                }
                Err(Error::Others(e.to_string()))
            }
        }
    }

    fn delete(&self) -> Result<(), containerd_shim_wasm::sandbox::Error> {
        log::info!(">>> Deleting container {}", self.id);
        match container_exists(&self.rootdir, self.id.as_str()) {
            Ok(exists) => {
                if !exists {
                    return Ok(());
                }
            }
            Err(err) => {
                error!("could not find the container, skipping cleanup: {}", err);
                return Ok(());
            }
        }
        match load_container(&self.rootdir, self.id.as_str()) {
            Ok(mut container) => container.delete(true).unwrap(),
            Err(err) => {
                error!("could not find the container, skipping cleanup: {}", err);
                return Ok(());
            }
        }

        Ok(())
    }

    fn wait(&self, waiter: &Wait) -> Result<(), containerd_shim_wasm::sandbox::Error> {
        log::info!(">>> Waiting for container {}", self.id);
        let code = self.exit_code.clone();
        waiter.set_up_exit_code_wait(code)
    }
}

impl MyContainer {
    fn build_executor(&self) -> Result<Container> {
        let syscall = create_syscall();
        fs::create_dir_all(&self.rootdir)?;
        let container = ContainerBuilder::new(self.id.clone(), syscall.as_ref())
            .with_executor(vec![Box::<DefaultExecutor>::default()])?
            .with_root_path(self.rootdir.clone())?
            .as_init(&self.bundle)
            .with_systemd(false)
            .build()?;
        Ok(container)
    }
}

fn container_exists<P: AsRef<Path>>(root_path: P, container_id: &str) -> Result<bool> {
    let container_root = construct_container_root(root_path, container_id)?;
    Ok(container_root.exists())
}

fn construct_container_root<P: AsRef<Path>>(root_path: P, container_id: &str) -> Result<PathBuf> {
    let root_path = fs::canonicalize(&root_path).with_context(|| {
        format!(
            "failed to canonicalize {} for container {}",
            root_path.as_ref().display(),
            container_id
        )
    })?;
    Ok(root_path.join(container_id))
}

fn load_container<P: AsRef<Path>>(root_path: P, container_id: &str) -> Result<Container> {
    let container_root = construct_container_root(root_path, container_id)?;
    if !container_root.exists() {
        bail!("container {} does not exist.", container_id)
    }

    Container::load(container_root)
        .with_context(|| format!("could not load state for container {container_id}"))
}

impl EngineGetter for MyContainer {
    type E = ();
    fn new_engine() -> Result<Self::E, Error> {
        Ok(())
    }
}

fn main() {
    shim::run::<ShimCli<MyContainer, _>>("io.containerd.youki.v1", None);
}
