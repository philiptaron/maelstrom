use anyhow::Result;
use clap::Args;
use maelstrom_base::{
    ClientJobId, JobCompleted, JobEffects, JobError, JobOutcome, JobOutcomeResult, JobOutputResult,
    JobStatus,
};
use maelstrom_client::{
    CacheDir, Client, ClientBgProcess, ContainerImageDepotDir, ProjectDir, StateDir,
};
use maelstrom_macro::Config;
use maelstrom_run::spec::job_spec_iter_from_reader;
use maelstrom_util::{
    config::common::{BrokerAddr, CacheSize, InlineLimit, LogLevel, Slots},
    fs::Fs,
    log,
    process::{ExitCode, ExitCodeAccumulator},
    root::{Root, RootBuf},
};
use std::{
    env,
    io::{self, Read, Write as _},
    path::PathBuf,
    sync::Arc,
    sync::{Condvar, Mutex},
};
use xdg::BaseDirectories;

#[derive(Config, Debug)]
pub struct Config {
    /// Socket address of broker. If not provided, all jobs will be run locally.
    #[config(
        option,
        short = 'b',
        value_name = "SOCKADDR",
        default = r#""standalone mode""#
    )]
    pub broker: Option<BrokerAddr>,

    /// Minimum log level to output.
    #[config(short = 'l', value_name = "LEVEL", default = r#""info""#)]
    pub log_level: LogLevel,

    /// Directory in which to put cached container images.
    #[config(
        value_name = "PATH",
        default = r#"|bd: &BaseDirectories| {
            bd.get_cache_home()
                .parent()
                .unwrap()
                .join("container/")
                .into_os_string()
                .into_string()
                .unwrap()
        }"#
    )]
    pub container_image_depot_root: RootBuf<ContainerImageDepotDir>,

    /// Directory for state that persists between runs, including the client's log file.
    #[config(
        value_name = "PATH",
        default = r#"|bd: &BaseDirectories| {
            bd.get_state_home()
                .into_os_string()
                .into_string()
                .unwrap()
        }"#
    )]
    pub state_root: RootBuf<StateDir>,

    /// Directory to use for the cache. The local worker's cache will be contained within it.
    #[config(
        value_name = "PATH",
        default = r#"|bd: &BaseDirectories| {
            bd.get_cache_home()
                .into_os_string()
                .into_string()
                .unwrap()
        }"#
    )]
    pub cache_root: RootBuf<CacheDir>,

    /// The target amount of disk space to use for the cache. This bound won't be followed
    /// strictly, so it's best to be conservative. SI and binary suffixes are supported.
    #[config(
        value_name = "BYTES",
        default = "CacheSize::default()",
        next_help_heading = "Local Worker Options"
    )]
    pub cache_size: CacheSize,

    /// The maximum amount of bytes to return inline for captured stdout and stderr.
    #[config(value_name = "BYTES", default = "InlineLimit::default()")]
    pub inline_limit: InlineLimit,

    /// The number of job slots available.
    #[config(value_name = "N", default = "Slots::default()")]
    pub slots: Slots,
}

#[derive(Args)]
#[command(next_help_heading = "Other Command-Line Options")]
pub struct ExtraCommandLineOptions {
    #[arg(
        long,
        short = 'f',
        value_name = "PATH",
        help = "Read the job specifications from the provided file, instead of from standard \
            input."
    )]
    pub file: Option<PathBuf>,
}

fn print_effects(
    cjid: ClientJobId,
    JobEffects {
        stdout,
        stderr,
        duration: _,
    }: JobEffects,
) -> Result<()> {
    match stdout {
        JobOutputResult::None => {}
        JobOutputResult::Inline(bytes) => {
            io::stdout().lock().write_all(&bytes)?;
        }
        JobOutputResult::Truncated { first, truncated } => {
            io::stdout().lock().write_all(&first)?;
            io::stdout().lock().flush()?;
            eprintln!("job {cjid}: stdout truncated, {truncated} bytes lost");
        }
    }
    match stderr {
        JobOutputResult::None => {}
        JobOutputResult::Inline(bytes) => {
            io::stderr().lock().write_all(&bytes)?;
        }
        JobOutputResult::Truncated { first, truncated } => {
            io::stderr().lock().write_all(&first)?;
            eprintln!("job {cjid}: stderr truncated, {truncated} bytes lost");
        }
    }
    Ok(())
}

fn visitor(res: Result<(ClientJobId, JobOutcomeResult)>, tracker: Arc<JobTracker>) {
    let exit_code = match res {
        Ok((cjid, Ok(JobOutcome::Completed(JobCompleted { status, effects })))) => {
            print_effects(cjid, effects).ok();
            match status {
                JobStatus::Exited(0) => ExitCode::SUCCESS,
                JobStatus::Exited(code) => {
                    io::stdout().lock().flush().ok();
                    eprintln!("job {cjid}: exited with code {code}");
                    ExitCode::from(code)
                }
                JobStatus::Signaled(signum) => {
                    io::stdout().lock().flush().ok();
                    eprintln!("job {cjid}: killed by signal {signum}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok((cjid, Ok(JobOutcome::TimedOut(effects)))) => {
            print_effects(cjid, effects).ok();
            io::stdout().lock().flush().ok();
            eprintln!("job {cjid}: timed out");
            ExitCode::FAILURE
        }
        Ok((cjid, Err(JobError::Execution(err)))) => {
            eprintln!("job {cjid}: execution error: {err}");
            ExitCode::FAILURE
        }
        Ok((cjid, Err(JobError::System(err)))) => {
            eprintln!("job {cjid}: system error: {err}");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("remote error: {err}");
            ExitCode::FAILURE
        }
    };
    tracker.job_completed(exit_code);
}

#[derive(Default)]
struct JobTracker {
    condvar: Condvar,
    outstanding: Mutex<usize>,
    accum: ExitCodeAccumulator,
}

impl JobTracker {
    fn add_outstanding(&self) {
        let mut locked = self.outstanding.lock().unwrap();
        *locked += 1;
    }

    fn job_completed(&self, exit_code: ExitCode) {
        let mut locked = self.outstanding.lock().unwrap();
        *locked -= 1;
        self.accum.add(exit_code);
        self.condvar.notify_one();
    }

    fn wait_for_outstanding(&self) {
        let mut locked = self.outstanding.lock().unwrap();
        while *locked > 0 {
            locked = self.condvar.wait(locked).unwrap();
        }
    }
}

fn main() -> Result<ExitCode> {
    let (config, extra_options): (_, ExtraCommandLineOptions) =
        Config::new_with_extra_from_args("maelstrom/run", "MAELSTROM_RUN", env::args())?;

    let bg_proc = ClientBgProcess::new_from_fork(config.log_level)?;

    log::run_with_logger(config.log_level, |log| {
        let fs = Fs::new();
        let reader: Box<dyn Read> = match extra_options.file {
            Some(path) => Box::new(fs.open_file(path)?),
            None => Box::new(io::stdin().lock()),
        };
        let tracker = Arc::new(JobTracker::default());
        fs.create_dir_all(&config.cache_root)?;
        fs.create_dir_all(&config.state_root)?;
        fs.create_dir_all(&config.container_image_depot_root)?;
        let client = Client::new(
            bg_proc,
            config.broker,
            Root::<ProjectDir>::new(".".as_ref()),
            config.state_root,
            config.container_image_depot_root,
            config.cache_root,
            config.cache_size,
            config.inline_limit,
            config.slots,
            log,
        )?;
        let job_specs = job_spec_iter_from_reader(reader, |layer| client.add_layer(layer));
        for job_spec in job_specs {
            let tracker = tracker.clone();
            tracker.add_outstanding();
            client.add_job(job_spec?, move |res| visitor(res, tracker))?;
        }
        tracker.wait_for_outstanding();
        Ok(tracker.accum.get())
    })
}
