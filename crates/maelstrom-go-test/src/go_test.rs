use crate::{GoPackage, GoPackageId, GoTestArtifact};
use anyhow::Result;
use maelstrom_util::fs::Fs;
use maelstrom_util::process::ExitCode;
use std::ffi::OsStr;
use std::os::unix::process::ExitStatusExt as _;
use std::{
    fmt,
    io::Read as _,
    path::Path,
    process::{Command, Stdio},
    str,
    sync::mpsc,
    thread,
};

#[derive(Debug)]
pub struct BuildError {
    pub stderr: String,
    pub exit_code: ExitCode,
}

impl std::error::Error for BuildError {}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "go test exited with {:?}\nstderr:\n{}",
            self.exit_code, self.stderr
        )
    }
}

pub struct WaitHandle {
    handle: thread::JoinHandle<Result<()>>,
}

impl WaitHandle {
    pub fn wait(self) -> Result<()> {
        self.handle.join().unwrap()
    }
}

pub(crate) struct TestArtifactStream {
    recv: mpsc::Receiver<GoTestArtifact>,
}

impl Iterator for TestArtifactStream {
    type Item = Result<GoTestArtifact>;

    fn next(&mut self) -> Option<Self::Item> {
        self.recv.recv().ok().map(Ok)
    }
}

fn go_build(dir: &Path) -> Result<()> {
    let mut child = Command::new("go")
        .current_dir(dir)
        .arg("test")
        .arg("-c")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let mut stdout = child.stdout.take().unwrap();
    let stdout_handle = thread::spawn(move || -> Result<String> {
        let mut stdout_string = String::new();
        stdout.read_to_string(&mut stdout_string)?;
        Ok(stdout_string)
    });

    let mut stderr = child.stderr.take().unwrap();
    let stderr_handle = thread::spawn(move || -> Result<String> {
        let mut stderr_string = String::new();
        stderr.read_to_string(&mut stderr_string)?;
        Ok(stderr_string)
    });

    let _stdout = stdout_handle.join().unwrap()?;
    let stderr = stderr_handle.join().unwrap()?;

    let exit_status = child.wait()?;
    if exit_status.success() {
        Ok(())
    } else {
        // Do like bash does and encode the signal in the exit code
        let exit_code = exit_status
            .code()
            .unwrap_or_else(|| 128 + exit_status.signal().unwrap());
        Err(BuildError {
            stderr,
            exit_code: ExitCode::from(exit_code as u8),
        }
        .into())
    }
}

fn multi_go_build(packages: Vec<GoPackage>, send: mpsc::Sender<GoTestArtifact>) -> Result<()> {
    let mut handles = vec![];
    for p in packages {
        let send_clone = send.clone();
        handles.push(thread::spawn(move || -> Result<()> {
            go_build(&p.package_dir)?;
            let _ = send_clone.send(GoTestArtifact {
                id: p.id.clone(),
                path: p.package_dir.join(format!("{}.test", p.id.short_name())),
            });
            Ok(())
        }));
    }
    for handle in handles {
        handle.join().unwrap()?;
    }
    Ok(())
}

pub(crate) fn build_and_collect(
    _color: bool,
    packages: Vec<&GoPackage>,
) -> Result<(WaitHandle, TestArtifactStream)> {
    let paths = packages.into_iter().cloned().collect();
    let (send, recv) = mpsc::channel();
    let handle = thread::spawn(move || multi_go_build(paths, send));
    Ok((WaitHandle { handle }, TestArtifactStream { recv }))
}

pub fn get_cases_from_binary(binary: &Path, filter: &Option<String>) -> Result<Vec<String>> {
    let filter = filter.as_ref().map(|s| s.as_str()).unwrap_or(".");

    let output = Command::new(binary)
        .arg(format!("-test.list={filter}"))
        .output()?;
    Ok(str::from_utf8(&output.stdout)?
        .split('\n')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_owned())
        .collect())
}

fn go_list(dir: &Path) -> Result<String> {
    let output = Command::new("go").current_dir(dir).arg("list").output()?;
    Ok(str::from_utf8(&output.stdout)?.trim().into())
}

pub(crate) fn find_packages(dir: &Path) -> Result<Vec<GoPackage>> {
    let dir = dir.canonicalize()?;
    let iter = Fs.walk(&dir).filter(|path| {
        path.as_ref()
            .is_ok_and(|p| p.file_name() == Some(OsStr::new("go.mod")))
    });
    let mut packages = vec![];
    for go_mod in iter {
        let go_mod = go_mod?;
        let package_dir = go_mod.parent().unwrap().to_owned();
        let module_name = go_list(&package_dir)?;
        packages.push(GoPackage {
            id: GoPackageId(module_name),
            package_dir,
        });
    }
    Ok(packages)
}
