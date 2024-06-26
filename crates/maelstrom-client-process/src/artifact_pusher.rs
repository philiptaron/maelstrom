use anyhow::{anyhow, Context as _, Result};
use maelstrom_base::{
    proto::{ArtifactPusherToBroker, BrokerToArtifactPusher, Hello},
    Sha256Digest,
};
use maelstrom_client_base::ArtifactUploadProgress;
use maelstrom_util::{async_fs::Fs, config::common::BrokerAddr, net};
use std::pin::{pin, Pin};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt as _},
    net::TcpStream,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
    task::JoinSet,
};

struct UploadProgress {
    size: u64,
    progress: AtomicU64,
}

#[derive(Clone, Default)]
pub struct ArtifactUploadTracker {
    uploads: Arc<Mutex<HashMap<String, Arc<UploadProgress>>>>,
}

impl ArtifactUploadTracker {
    async fn new_upload(&self, name: impl Into<String>, size: u64) -> Arc<UploadProgress> {
        let mut uploads = self.uploads.lock().await;
        let prog = Arc::new(UploadProgress {
            size,
            progress: AtomicU64::new(0),
        });
        uploads.insert(name.into(), prog.clone());
        prog
    }

    async fn remove_upload(&self, name: &str) {
        let mut uploads = self.uploads.lock().await;
        uploads.remove(name);
    }

    pub async fn get_artifact_upload_progress(&self) -> Vec<ArtifactUploadProgress> {
        let uploads = self.uploads.lock().await;
        uploads
            .iter()
            .map(|(name, p)| ArtifactUploadProgress {
                name: name.clone(),
                size: p.size,
                progress: p.progress.load(Ordering::Acquire),
            })
            .collect()
    }
}

struct UploadProgressReader<ReadT> {
    prog: Arc<UploadProgress>,
    read: ReadT,
}

impl<ReadT> UploadProgressReader<ReadT> {
    fn new(prog: Arc<UploadProgress>, read: ReadT) -> Self {
        Self { prog, read }
    }
}

impl<ReadT: AsyncRead + Unpin> AsyncRead for UploadProgressReader<ReadT> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        dst: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let start_len = dst.filled().len();
        let me = self.get_mut();
        let result = AsyncRead::poll_read(pin!(&mut me.read), cx, dst);
        let amount_read = dst.filled().len() - start_len;
        me.prog
            .progress
            .fetch_add(amount_read as u64, Ordering::AcqRel);
        result
    }
}

fn construct_upload_name(digest: &Sha256Digest, path: &Path) -> String {
    let digest_string = digest.to_string();
    let short_digest = &digest_string[digest_string.len() - 7..];
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    format!("{short_digest} {file_name}")
}

async fn push_one_artifact(
    upload_tracker: ArtifactUploadTracker,
    broker_addr: BrokerAddr,
    path: PathBuf,
    digest: Sha256Digest,
) -> Result<()> {
    let mut stream = TcpStream::connect(broker_addr.inner()).await?;
    net::write_message_to_async_socket(&mut stream, Hello::ArtifactPusher).await?;

    let fs = Fs::new();
    let file = fs.open_file(&path).await?;
    let size = file.metadata().await?.len();

    let upload_name = construct_upload_name(&digest, &path);
    let prog = upload_tracker.new_upload(&upload_name, size).await;

    let mut file = UploadProgressReader::new(prog, file.chain(io::repeat(0)).take(size));

    net::write_message_to_async_socket(&mut stream, ArtifactPusherToBroker(digest, size)).await?;
    let copied = io::copy(&mut file, &mut stream).await?;
    assert_eq!(copied, size);

    let BrokerToArtifactPusher(resp) = net::read_message_from_async_socket(&mut stream).await?;

    upload_tracker.remove_upload(&upload_name).await;
    resp.map_err(|e| anyhow!("Error from broker: {e}"))
}

pub struct Message {
    pub path: PathBuf,
    pub digest: Sha256Digest,
}

pub type Sender = UnboundedSender<Message>;
pub type Receiver = UnboundedReceiver<Message>;

pub fn channel() -> (Sender, Receiver) {
    mpsc::unbounded_channel()
}

pub fn start_task(
    join_set: &mut JoinSet<Result<()>>,
    mut receiver: Receiver,
    broker_addr: BrokerAddr,
    upload_tracker: ArtifactUploadTracker,
) {
    join_set.spawn(async move {
        // When this join_set gets destroyed, all outstanding artifact pusher tasks will be
        // canceled. That will happen either when our sender is closed, or when our own task is
        // canceled. In either case, it means the process is shutting down.
        //
        // We have to be careful not to let the join_set grow indefinitely. This is why we select!
        // below. We always wait on the join_set's join_next, and ignore the results. This way we
        // immediately clean up when a task completes.
        let mut join_set = JoinSet::new();
        loop {
            tokio::select! {
                Some(res) = join_set.join_next() => {
                    res.unwrap()?; // We don't expect JoinErrors.
                },
                res = receiver.recv() => {
                    let Some(msg) = res else { break; };
                    let upload_tracker = upload_tracker.clone();

                    join_set.spawn(async move {
                        push_one_artifact(upload_tracker, broker_addr, msg.path, msg.digest)
                            .await.with_context(|| "Pushing artifact")
                    });
                }
            }
        }
        Ok(())
    });
}
