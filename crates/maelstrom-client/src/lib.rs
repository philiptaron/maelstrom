pub mod spec;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use maelstrom_base::{
    proto::{
        ArtifactPusherToBroker, BrokerToArtifactPusher, BrokerToClient, ClientToBroker, Hello,
    },
    stats::JobStateCounts,
    ArtifactType, ClientJobId, JobSpec, JobStringResult, Layer, PrefixOptions, Sha256Digest,
    Utf8Path, Utf8PathBuf,
};
use maelstrom_container::ContainerImageDepot;
use maelstrom_util::{
    config::BrokerAddr, ext::OptionExt as _, fs::Fs, io::FixedSizeReader,
    manifest::ManifestBuilder, net,
};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use serde_with::{serde_as, DisplayFromStr};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    io::{self, Read as _, Seek as _, SeekFrom, Write as _},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::SystemTime,
};

fn push_one_artifact(broker_addr: BrokerAddr, path: PathBuf, digest: Sha256Digest) -> Result<()> {
    let mut stream = TcpStream::connect(broker_addr.inner())?;
    net::write_message_to_socket(&mut stream, Hello::ArtifactPusher)?;

    let fs = Fs::new();
    let file = fs.open_file(path)?;
    let size = file.metadata()?.len();
    let mut file = FixedSizeReader::new(file, size);

    net::write_message_to_socket(&mut stream, ArtifactPusherToBroker(digest, size))?;
    let copied = io::copy(&mut file, &mut stream)?;
    assert_eq!(copied, size);
    let BrokerToArtifactPusher(resp) = net::read_message_from_socket(&mut stream)?;
    resp.map_err(|e| anyhow!("Error from broker: {e}"))
}

fn calculate_digest(path: &Path) -> Result<(SystemTime, Sha256Digest)> {
    let fs = Fs::new();
    let mut hasher = Sha256::new();
    let mut f = fs.open_file(path)?;
    std::io::copy(&mut f, &mut hasher)?;
    let mtime = f.metadata()?.modified()?;

    Ok((mtime, Sha256Digest::new(hasher.finalize().into())))
}

enum DispatcherMessage {
    BrokerToClient(BrokerToClient),
    AddArtifact(PathBuf, Sha256Digest),
    AddJob(JobSpec, JobResponseHandler),
    GetJobStateCounts(SyncSender<JobStateCounts>),
    Stop,
}

struct ArtifactPushRequest {
    path: PathBuf,
    digest: Sha256Digest,
}

pub struct ArtifactPusher {
    broker_addr: BrokerAddr,
    receiver: Receiver<ArtifactPushRequest>,
}

impl ArtifactPusher {
    fn new(broker_addr: BrokerAddr, receiver: Receiver<ArtifactPushRequest>) -> Self {
        Self {
            broker_addr,
            receiver,
        }
    }

    /// Processes one request. In order to drive the ArtifactPusher, this should be called in a loop
    /// until the function return false
    pub fn process_one<'a, 'b>(&mut self, scope: &'a thread::Scope<'b, '_>) -> bool
    where
        'a: 'b,
    {
        if let Ok(msg) = self.receiver.recv() {
            let broker_addr = self.broker_addr;
            // N.B. We are ignoring this Result<_>
            scope.spawn(move || push_one_artifact(broker_addr, msg.path, msg.digest));
            true
        } else {
            false
        }
    }
}

pub struct Dispatcher {
    receiver: Receiver<DispatcherMessage>,
    stream: TcpStream,
    artifact_pusher: SyncSender<ArtifactPushRequest>,
    stop_when_all_completed: bool,
    next_client_job_id: u32,
    artifacts: HashMap<Sha256Digest, PathBuf>,
    handlers: HashMap<ClientJobId, JobResponseHandler>,
    stats_reqs: VecDeque<SyncSender<JobStateCounts>>,
}

impl Dispatcher {
    fn new(
        receiver: Receiver<DispatcherMessage>,
        stream: TcpStream,
        artifact_pusher: SyncSender<ArtifactPushRequest>,
    ) -> Self {
        Self {
            receiver,
            stream,
            artifact_pusher,
            stop_when_all_completed: false,
            next_client_job_id: 0u32,
            artifacts: Default::default(),
            handlers: Default::default(),
            stats_reqs: Default::default(),
        }
    }

    /// Processes one request. In order to drive the dispatcher, this should be called in a loop
    /// until the function return false
    pub fn process_one(&mut self) -> Result<bool> {
        let msg = self.receiver.recv()?;
        self.handle_message(msg)
    }

    pub fn try_process_one(&mut self) -> Result<bool> {
        let msg = self.receiver.try_recv()?;
        self.handle_message(msg)
    }

    fn handle_message(&mut self, msg: DispatcherMessage) -> Result<bool> {
        match msg {
            DispatcherMessage::BrokerToClient(BrokerToClient::JobResponse(cjid, result)) => {
                self.handlers.remove(&cjid).unwrap()(cjid, result)?;
                if self.stop_when_all_completed && self.handlers.is_empty() {
                    return Ok(false);
                }
            }
            DispatcherMessage::BrokerToClient(BrokerToClient::TransferArtifact(digest)) => {
                let path = self
                    .artifacts
                    .get(&digest)
                    .unwrap_or_else(|| {
                        panic!("got request for unknown artifact with digest {digest}")
                    })
                    .clone();
                self.artifact_pusher
                    .send(ArtifactPushRequest { path, digest })?;
            }
            DispatcherMessage::BrokerToClient(BrokerToClient::StatisticsResponse(_)) => {
                unimplemented!("this client doesn't send statistics requests")
            }
            DispatcherMessage::BrokerToClient(BrokerToClient::JobStateCountsResponse(res)) => {
                self.stats_reqs.pop_front().unwrap().send(res).ok();
            }
            DispatcherMessage::AddArtifact(path, digest) => {
                self.artifacts.insert(digest, path);
            }
            DispatcherMessage::AddJob(spec, handler) => {
                let cjid = self.next_client_job_id.into();
                self.handlers.insert(cjid, handler).assert_is_none();
                self.next_client_job_id = self.next_client_job_id.checked_add(1).unwrap();
                net::write_message_to_socket(
                    &mut self.stream,
                    ClientToBroker::JobRequest(cjid, spec),
                )?;
            }
            DispatcherMessage::Stop => {
                if self.handlers.is_empty() {
                    return Ok(false);
                }
                self.stop_when_all_completed = true;
            }
            DispatcherMessage::GetJobStateCounts(sender) => {
                net::write_message_to_socket(
                    &mut self.stream,
                    ClientToBroker::JobStateCountsRequest,
                )?;
                self.stats_reqs.push_back(sender);
            }
        }
        Ok(true)
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u32)]
pub enum DigestRepositoryVersion {
    #[default]
    V0 = 0,
}

#[serde_as]
#[derive(Serialize, Deserialize)]
struct DigestRepositoryEntry {
    #[serde_as(as = "DisplayFromStr")]
    digest: Sha256Digest,
    mtime: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Default)]
struct DigestRepositoryContents {
    pub version: DigestRepositoryVersion,
    pub digests: HashMap<PathBuf, DigestRepositoryEntry>,
}

impl DigestRepositoryContents {
    fn from_str(s: &str) -> Result<Self> {
        Ok(toml::from_str(s)?)
    }

    fn to_pretty_string(&self) -> String {
        toml::to_string_pretty(self).unwrap()
    }
}

const CACHED_IMAGE_FILE_NAME: &str = "maelstrom-cached-digests.toml";

struct DigestRespository {
    fs: Fs,
    path: PathBuf,
}

impl DigestRespository {
    fn new(path: &Path) -> Self {
        Self {
            fs: Fs::new(),
            path: path.into(),
        }
    }

    fn add(&self, path: PathBuf, mtime: SystemTime, digest: Sha256Digest) -> Result<()> {
        self.fs.create_dir_all(&self.path)?;
        let mut file = self
            .fs
            .open_or_create_file(self.path.join(CACHED_IMAGE_FILE_NAME))?;
        file.lock_exclusive()?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let mut digests = DigestRepositoryContents::from_str(&contents).unwrap_or_default();
        digests.digests.insert(
            path,
            DigestRepositoryEntry {
                mtime: mtime.into(),
                digest,
            },
        );

        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(digests.to_pretty_string().as_bytes())?;

        Ok(())
    }

    fn get(&self, path: &PathBuf) -> Result<Option<Sha256Digest>> {
        let Some(contents) = self
            .fs
            .read_to_string_if_exists(self.path.join(CACHED_IMAGE_FILE_NAME))?
        else {
            return Ok(None);
        };
        let mut digests = DigestRepositoryContents::from_str(&contents).unwrap_or_default();
        let Some(entry) = digests.digests.remove(path) else {
            return Ok(None);
        };
        let current_mtime: DateTime<Utc> = self.fs.metadata(path)?.modified()?.into();
        Ok((current_mtime == entry.mtime).then_some(entry.digest))
    }
}

#[test]
fn digest_repository_simple_add_get() {
    let fs = Fs::new();
    let tmp_dir = tempfile::tempdir().unwrap();
    let repo = DigestRespository::new(tmp_dir.path());

    let foo_path = tmp_dir.path().join("foo.tar");
    fs.write(&foo_path, "foo").unwrap();
    let (mtime, digest) = calculate_digest(&foo_path).unwrap();
    repo.add(foo_path.clone(), mtime, digest.clone()).unwrap();

    assert_eq!(repo.get(&foo_path).unwrap(), Some(digest));
}

#[test]
fn digest_repository_simple_add_get_after_modify() {
    let fs = Fs::new();
    let tmp_dir = tempfile::tempdir().unwrap();
    let repo = DigestRespository::new(tmp_dir.path());

    let foo_path = tmp_dir.path().join("foo.tar");
    fs.write(&foo_path, "foo").unwrap();
    let (mtime, digest) = calculate_digest(&foo_path).unwrap();
    repo.add(foo_path.clone(), mtime, digest.clone()).unwrap();

    // apparently depending on the file-system mtime can have up to a 10ms granularity
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs.write(&foo_path, "bar").unwrap();

    assert_eq!(repo.get(&foo_path).unwrap(), None);
}

pub struct SocketReader {
    stream: TcpStream,
    channel: SyncSender<DispatcherMessage>,
}

impl SocketReader {
    fn new(stream: TcpStream, channel: SyncSender<DispatcherMessage>) -> Self {
        Self { stream, channel }
    }

    pub fn process_one(&mut self) -> bool {
        let Ok(msg) = net::read_message_from_socket(&mut self.stream) else {
            return false;
        };
        self.channel
            .send(DispatcherMessage::BrokerToClient(msg))
            .is_ok()
    }
}

pub struct ClientDeps {
    pub dispatcher: Dispatcher,
    pub artifact_pusher: ArtifactPusher,
    pub socket_reader: SocketReader,
    dispatcher_sender: SyncSender<DispatcherMessage>,
}

impl ClientDeps {
    fn new(broker_addr: BrokerAddr) -> Result<Self> {
        let mut stream = TcpStream::connect(broker_addr.inner())?;
        net::write_message_to_socket(&mut stream, Hello::Client)?;

        let (dispatcher_sender, dispatcher_receiver) = mpsc::sync_channel(1000);
        let (artifact_send, artifact_recv) = mpsc::sync_channel(1000);
        let stream_clone = stream.try_clone()?;
        Ok(Self {
            dispatcher: Dispatcher::new(dispatcher_receiver, stream_clone, artifact_send),
            artifact_pusher: ArtifactPusher::new(broker_addr, artifact_recv),
            socket_reader: SocketReader::new(stream, dispatcher_sender.clone()),
            dispatcher_sender,
        })
    }
}

pub trait ClientDriver {
    fn drive(&mut self, deps: ClientDeps);
    fn stop(&mut self) -> Result<()>;
}

#[derive(Default)]
pub struct DefaultClientDriver {
    handle: Option<JoinHandle<Result<()>>>,
}

impl ClientDriver for DefaultClientDriver {
    fn drive(&mut self, mut deps: ClientDeps) {
        assert!(self.handle.is_none());
        self.handle = Some(thread::spawn(move || {
            thread::scope(|scope| {
                let dispatcher_handle = scope.spawn(move || {
                    while deps.dispatcher.process_one()? {}
                    deps.dispatcher.stream.shutdown(std::net::Shutdown::Both)?;
                    Ok(())
                });
                scope.spawn(move || while deps.artifact_pusher.process_one(scope) {});
                scope.spawn(move || while deps.socket_reader.process_one() {});
                dispatcher_handle.join().unwrap()
            })
        }));
    }

    fn stop(&mut self) -> Result<()> {
        self.handle.take().unwrap().join().unwrap()
    }
}

#[derive(Default)]
struct PathHasher {
    hasher: Sha256,
}

impl PathHasher {
    fn new() -> Self {
        Self::default()
    }

    fn hash_path(&mut self, path: &Utf8Path) {
        self.hasher.update(path.as_str().as_bytes());
    }

    fn finish(self) -> Sha256Digest {
        Sha256Digest::new(self.hasher.finalize().into())
    }
}

fn calculate_manifest_entry_path(path: &Utf8Path, prefix_options: &PrefixOptions) -> Utf8PathBuf {
    let mut path = path.to_owned();
    if let Some(prefix) = &prefix_options.strip_prefix {
        if let Ok(new_path) = path.strip_prefix(prefix) {
            path = new_path.to_owned();
        }
    }
    if let Some(prefix) = &prefix_options.prepend_prefix {
        if path.is_absolute() {
            path = prefix.join(path.strip_prefix("/").unwrap());
        } else {
            path = prefix.join(path);
        }
    }
    path
}

pub type JobResponseHandler =
    Box<dyn FnOnce(ClientJobId, JobStringResult) -> Result<()> + Send + Sync>;

pub const MANIFEST_DIR: &str = "maelstrom-manifests";

pub struct Client {
    dispatcher_sender: SyncSender<DispatcherMessage>,
    driver: Box<dyn ClientDriver + Send + Sync>,
    digest_repo: DigestRespository,
    container_image_depot: ContainerImageDepot,
    processed_artifact_paths: HashSet<PathBuf>,
    cache_dir: PathBuf,
    project_dir: PathBuf,
}

impl Client {
    pub fn new<DriverT: ClientDriver + 'static + Send + Sync>(
        mut driver: DriverT,
        broker_addr: BrokerAddr,
        project_dir: impl AsRef<Path>,
        cache_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let deps = ClientDeps::new(broker_addr)?;
        let dispatcher_sender = deps.dispatcher_sender.clone();
        driver.drive(deps);

        let fs = Fs::new();
        fs.create_dir_all(cache_dir.as_ref().join(MANIFEST_DIR))?;

        Ok(Client {
            dispatcher_sender,
            driver: Box::new(driver),
            digest_repo: DigestRespository::new(cache_dir.as_ref()),
            container_image_depot: ContainerImageDepot::new(project_dir.as_ref())?,
            processed_artifact_paths: HashSet::default(),
            cache_dir: cache_dir.as_ref().to_owned(),
            project_dir: project_dir.as_ref().to_owned(),
        })
    }

    pub fn add_artifact(&mut self, path: &Path) -> Result<Sha256Digest> {
        let fs = Fs::new();
        let path = fs.canonicalize(path)?;

        let digest = if let Some(digest) = self.digest_repo.get(&path)? {
            digest
        } else {
            let (mtime, digest) = calculate_digest(&path)?;
            self.digest_repo.add(path.clone(), mtime, digest.clone())?;
            digest
        };
        if !self.processed_artifact_paths.contains(&path) {
            self.dispatcher_sender
                .send(DispatcherMessage::AddArtifact(path.clone(), digest.clone()))?;
            self.processed_artifact_paths.insert(path);
        }
        Ok(digest)
    }

    fn build_manifest_path(&self, name: &impl fmt::Display) -> PathBuf {
        self.cache_dir
            .join(MANIFEST_DIR)
            .join(format!("{name}.manifest"))
    }

    fn build_manifest(
        &mut self,
        paths: impl Iterator<Item = Result<impl AsRef<Path>>>,
        prefix_options: PrefixOptions,
    ) -> Result<PathBuf> {
        let fs = Fs::new();
        let tmp_file_path = self.build_manifest_path(&".temp");
        let manifest_file = fs.create_file(&tmp_file_path)?;
        let data_upload = |path: &_| self.add_artifact(path);
        let mut builder =
            ManifestBuilder::new(manifest_file, false /* follow_symlinks */, data_upload)?;
        let mut path_hasher = PathHasher::new();
        for path in paths {
            let std_path = path?;
            let path =
                Utf8Path::from_path(std_path.as_ref()).ok_or_else(|| anyhow!("non-utf8 path"))?;
            path_hasher.hash_path(path);
            let dest = calculate_manifest_entry_path(path, &prefix_options);
            builder.add_file(path, dest)?;
        }
        drop(builder);

        let manifest_path = self.build_manifest_path(&path_hasher.finish());
        fs.rename(tmp_file_path, &manifest_path)?;
        Ok(manifest_path)
    }

    pub fn add_layer(&mut self, layer: Layer) -> Result<(Sha256Digest, ArtifactType)> {
        match layer {
            Layer::Tar { path } => Ok((self.add_artifact(path.as_std_path())?, ArtifactType::Tar)),
            Layer::Paths {
                paths,
                prefix_options,
            } => {
                let manifest_path =
                    self.build_manifest(paths.iter().map(|p| Ok(p)), prefix_options)?;
                Ok((self.add_artifact(&manifest_path)?, ArtifactType::Manifest))
            }
            _ => Err(anyhow!("unimplemented layer type")),
        }
    }

    pub fn container_image_depot_mut(&mut self) -> &mut ContainerImageDepot {
        &mut self.container_image_depot
    }

    pub fn add_job(&mut self, spec: JobSpec, handler: JobResponseHandler) {
        // We will only get an error if the dispatcher has closed its receiver, which will only
        // happen if it ran into an error. We'll get that error when we wait in
        // `wait_for_oustanding_job`.
        let _ = self
            .dispatcher_sender
            .send(DispatcherMessage::AddJob(spec, handler));
    }

    pub fn stop_accepting(&mut self) -> Result<()> {
        self.dispatcher_sender.send(DispatcherMessage::Stop)?;
        Ok(())
    }

    pub fn wait_for_outstanding_jobs(&mut self) -> Result<()> {
        self.stop_accepting().ok();
        self.driver.stop()?;
        Ok(())
    }

    pub fn get_job_state_counts_async(&mut self) -> Result<Receiver<JobStateCounts>> {
        let (sender, recv) = mpsc::sync_channel(1);
        self.dispatcher_sender
            .send(DispatcherMessage::GetJobStateCounts(sender))?;
        Ok(recv)
    }

    pub fn get_job_state_counts(&mut self) -> Result<JobStateCounts> {
        Ok(self.get_job_state_counts_async()?.recv()?)
    }
}
