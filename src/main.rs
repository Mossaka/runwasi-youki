use anyhow::{bail, Context, Result};
use libcontainer::syscall::syscall::SyscallType;
use libcontainer::workload::ExecutorError;
use nix::unistd::{dup, dup2};
use serde::{Deserialize, Serialize};
use youki_wasmedge_executor;
use std::fs::OpenOptions;
use std::os::fd::{IntoRawFd, RawFd};
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
use libc::{SIGINT, SIGKILL, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use libcontainer::{
    container::builder::ContainerBuilder, oci_spec::runtime::Spec
};

use log::error;
use nix::errno::Errno;
use nix::sys::wait::{waitid, Id as WaitID, WaitPidFlag, WaitStatus};

use libcontainer::container::{Container, ContainerStatus};
use libcontainer::signal::Signal;

type ExitCode = Arc<(Mutex<Option<(u32, DateTime<Utc>)>>, Condvar)>;
static DEFAULT_CONTAINER_ROOT_DIR: &str = "/run/containerd/youki";

pub struct MyContainer {
    exit_code: ExitCode,
    id: String,
    stdin: String,
    stdout: String,
    stderr: String,
    bundle: String,

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
        let cfg = cfg.unwrap();
        let bundle = cfg.get_bundle().unwrap_or_default();
        log::info!(">>> Bundle: {:?}", bundle);
        let namespace = cfg.get_namespace();
        log::info!(">>> Namespace: {:?}", namespace);
        let rootdir = determine_rootdir(bundle.as_str(), namespace).unwrap();
        log::info!(">>> Rootdir: {:?}", rootdir);
        MyContainer {
            id,
            exit_code: Arc::new((Mutex::new(None), Condvar::new())),
            stdin: cfg.get_stdin().unwrap_or_default(),
            stdout: cfg.get_stdout().unwrap_or_default(),
            stderr: cfg.get_stderr().unwrap_or_default(),
            bundle: bundle.clone(),
            rootdir,
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
                log::error!("failed to kill container: {}", e);
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

/// containerd can send an empty path or a non-existant path
/// In both these cases we should just assume that the stdio stream was not setup (intentionally)
/// Any other error is a real error.
fn maybe_open_stdio(path: &str) -> Result<Option<RawFd>, Error> {
    if path.is_empty() {
        return Ok(None);
    }
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => Ok(Some(f.into_raw_fd())),
        Err(err) => match err.kind() {
            ErrorKind::NotFound => Ok(None),
            _ => Err(err.into()),
        },
    }
}

impl MyContainer {
    fn build_executor(&self) -> Result<Container> {
        let syscall = SyscallType::default();
        fs::create_dir_all(&self.rootdir)?;
        // verify that roodir is created
        assert!(self.rootdir.exists());
        let stdin = maybe_open_stdio(self.stdin.as_str()).context("could not open stdin")?;
        let stdout = maybe_open_stdio(self.stdout.as_str()).context("could not open stdout")?;
        let stderr = maybe_open_stdio(self.stderr.as_str()).context("could not open stderr")?;

        if let Some(stdin) = stdin {
            let _ = dup(STDIN_FILENO)?;
            dup2(stdin, STDIN_FILENO)?;
        }

        if let Some(stdout) = stdout {
            let _ = dup(STDOUT_FILENO)?;
            dup2(stdout, STDOUT_FILENO)?;
        }

        if let Some(stderr) = stderr {
            let _ = dup(STDERR_FILENO)?;
            dup2(stderr, STDERR_FILENO)?;
        }

        let container = ContainerBuilder::new(self.id.clone(), syscall)
            .with_executor(Box::new(|spec: &Spec| -> Result<(), ExecutorError> {
                match youki_wasmedge_executor::get_executor()(spec) {
                    Ok(_) => return Ok(()),
                    Err(ExecutorError::CantHandle(_)) => (),
                    Err(err) => return Err(err),
                }
                libcontainer::workload::default::get_executor()(spec)
            }))
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
    let root_path = fs::canonicalize(&root_path)?;
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
