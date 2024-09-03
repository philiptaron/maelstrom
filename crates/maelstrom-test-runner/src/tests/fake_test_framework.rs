use crate::{
    metadata::TestMetadata,
    test_db::{TestDb, TestDbStore, TestDbStoreDeps},
    ui::{self},
    BuildDir, CollectTests, NoCaseMetadata, SimpleFilter, StringArtifactKey, StringPackage,
    TestArtifact, TestFilter, TestPackage, TestPackageId, Wait,
};
use anyhow::Result;
use maelstrom_base::{
    stats::JobState, JobCompleted, JobEffects, JobOutcome, JobOutputResult, JobTerminationStatus,
    Utf8PathBuf,
};
use maelstrom_client::spec::{JobSpec, LayerSpec};
use maelstrom_util::{fs::Fs, root::RootBuf};
use pretty_assertions::assert_eq;
use std::{
    cell::RefCell,
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

pub struct BinDir;

#[derive(Clone, Default)]
pub struct FakeTestCompleteCallback(
    pub Arc<Mutex<Option<Box<dyn FnOnce() + Send + Sync + 'static>>>>,
);

impl FakeTestCompleteCallback {
    pub fn set(&self, handler: impl FnOnce() + Send + Sync + 'static) {
        *self.0.lock().unwrap() = Some(Box::new(handler));
    }

    fn call(&self) {
        (self.0.lock().unwrap().take().unwrap())();
    }
}

impl fmt::Debug for FakeTestCompleteCallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt("<callback>", f)
    }
}

#[derive(Clone, Debug)]
pub struct FakeTestCase {
    pub name: String,
    pub ignored: bool,
    pub desired_state: JobState,
    pub expected_estimated_duration: Option<Duration>,
    pub outcome: JobOutcome,
    pub complete_at_end: bool,
    pub cb: FakeTestCompleteCallback,
}

impl FakeTestCase {
    fn timing(&self) -> Duration {
        let (JobOutcome::TimedOut(JobEffects { duration, .. })
        | JobOutcome::Completed(JobCompleted {
            effects: JobEffects { duration, .. },
            ..
        })) = self.outcome;
        duration
    }

    pub fn maybe_complete(&self) {
        if self.complete_at_end {
            self.cb.call();
        }
    }
}

impl Default for FakeTestCase {
    fn default() -> Self {
        Self {
            name: "".into(),
            ignored: false,
            desired_state: JobState::Complete,
            expected_estimated_duration: None,
            outcome: JobOutcome::Completed(JobCompleted {
                status: JobTerminationStatus::Exited(0),
                effects: JobEffects {
                    stdout: JobOutputResult::None,
                    stderr: JobOutputResult::Inline(Box::new(*b"this output should be ignored")),
                    duration: Duration::from_secs(1),
                },
            }),
            complete_at_end: false,
            cb: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FakeTestBinary {
    pub name: String,
    pub tests: Vec<FakeTestCase>,
}

impl FakeTestBinary {
    fn artifact_key(&self) -> StringArtifactKey {
        self.name.as_str().into()
    }
}

#[derive(Clone, Debug, Default)]
pub struct FakeTests {
    pub test_binaries: Vec<FakeTestBinary>,
}

impl FakeTests {
    pub fn create_binaries(&self, fs: &Fs, bin_path: &Path) {
        for bin in &self.test_binaries {
            let dest = bin_path.join(&bin.name);
            if !fs.exists(&dest) {
                fs.symlink("/proc/self/exe", dest).unwrap();
            }
        }
    }

    pub fn update_db(
        &self,
        mut db: TestDb<StringArtifactKey, NoCaseMetadata>,
    ) -> TestDb<StringArtifactKey, NoCaseMetadata> {
        struct FakeTestDbStoreDeps {
            bytes: RefCell<Option<Vec<u8>>>,
        }
        impl TestDbStoreDeps for FakeTestDbStoreDeps {
            fn read_to_string_if_exists(&self, _path: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(self
                    .bytes
                    .borrow()
                    .clone()
                    .map(|bytes| String::from_utf8(bytes).unwrap()))
            }
            fn create_dir_all(&self, _path: impl AsRef<Path>) -> Result<()> {
                Ok(())
            }
            fn write(&self, _path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
                self.bytes.borrow_mut().replace(contents.as_ref().to_vec());
                Ok(())
            }
        }
        db.retain_packages_and_artifacts(
            self.test_binaries
                .iter()
                .map(|binary| (binary.name.as_str(), [binary.artifact_key()])),
        );
        for binary in &self.test_binaries {
            db.update_artifact_cases(
                &binary.name,
                binary.artifact_key(),
                binary
                    .tests
                    .iter()
                    .map(|case| (case.name.clone(), NoCaseMetadata)),
            );
            for case in &binary.tests {
                db.add_timing(
                    &binary.name,
                    binary.artifact_key(),
                    &case.name,
                    !matches!(
                        &case.outcome,
                        JobOutcome::Completed(JobCompleted {
                            status: JobTerminationStatus::Exited(0),
                            ..
                        })
                    ),
                    case.timing(),
                );
            }
        }
        let db_store_deps = FakeTestDbStoreDeps {
            bytes: RefCell::new(None),
        };
        let db_store = TestDbStore::new(db_store_deps, RootBuf::new(PathBuf::from("")));
        db_store.save(db).unwrap();
        db_store.load().unwrap()
    }

    pub fn packages(&self) -> Vec<FakeTestPackage> {
        self.test_binaries
            .iter()
            .map(|b| FakeTestPackage {
                name: b.name.clone(),
                artifacts: vec![StringArtifactKey::from(b.name.as_ref())],
                id: FakePackageId(format!("{} 1.0.0", b.name)),
            })
            .collect()
    }

    fn artifacts(
        &self,
        bin_path: &Path,
        packages: Vec<&FakeTestPackage>,
    ) -> Vec<Result<FakeTestArtifact>> {
        let packages: HashSet<_> = packages.iter().map(|p| p.name()).collect();
        self.test_binaries
            .iter()
            .filter_map(|b| {
                if !packages.contains(b.name.as_str()) {
                    return None;
                }

                let exe = bin_path.join(&b.name);
                Some(Ok(FakeTestArtifact {
                    name: b.name.clone(),
                    tests: self.cases(&exe),
                    ignored_tests: self.ignored_cases(&exe),
                    path: exe,
                    package: FakePackageId(format!("{} 1.0.0", b.name)),
                }))
            })
            .collect()
    }

    fn cases(&self, binary: &Path) -> Vec<String> {
        let binary_name = binary.file_name().unwrap().to_str().unwrap();
        let binary = self.find_binary(binary_name);
        binary.tests.iter().map(|t| t.name.to_owned()).collect()
    }

    fn ignored_cases(&self, binary: &Path) -> Vec<String> {
        let binary_name = binary.file_name().unwrap().to_str().unwrap();
        let binary = self.find_binary(binary_name);
        binary
            .tests
            .iter()
            .filter(|&t| t.ignored)
            .map(|t| t.name.to_owned())
            .collect()
    }

    pub fn find_case_for_spec(&self, spec: JobSpec) -> &FakeTestCase {
        let binary_name = spec.program.file_name().unwrap();
        let binary = self.find_binary(binary_name);
        let case_name = spec
            .arguments
            .iter()
            .find(|a| !a.starts_with("--"))
            .unwrap();
        let case = binary.tests.iter().find(|c| &c.name == case_name).unwrap();
        assert_eq!(&spec.estimated_duration, &case.expected_estimated_duration);
        case
    }

    fn find_binary(&self, binary_name: &str) -> &FakeTestBinary {
        self.test_binaries
            .iter()
            .find(|b| b.name == binary_name)
            .unwrap_or_else(|| panic!("binary {binary_name} not found"))
    }

    pub fn find_case(&self, binary_name: &str, case: &str) -> &FakeTestCase {
        let binary = self.find_binary(binary_name);
        binary.tests.iter().find(|c| &c.name == case).unwrap()
    }
}

pub struct WaitForNothing;

impl Wait for WaitForNothing {
    fn wait(self) -> Result<()> {
        Ok(())
    }
}

pub struct TestCollector {
    pub tests: FakeTests,
    pub bin_path: RootBuf<BinDir>,
    pub target_dir: RootBuf<BuildDir>,
}

#[derive(Clone, Debug)]
pub struct FakeTestArtifact {
    name: String,
    tests: Vec<String>,
    ignored_tests: Vec<String>,
    path: PathBuf,
    package: FakePackageId,
}

#[derive(Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct FakePackageId(String);

impl TestPackageId for FakePackageId {}

impl TestArtifact for FakeTestArtifact {
    type ArtifactKey = StringArtifactKey;
    type PackageId = FakePackageId;
    type CaseMetadata = NoCaseMetadata;

    fn package(&self) -> FakePackageId {
        self.package.clone()
    }

    fn to_key(&self) -> StringArtifactKey {
        StringArtifactKey::from(self.name.as_ref())
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn list_tests(&self) -> Result<Vec<(String, NoCaseMetadata)>> {
        Ok(self
            .tests
            .iter()
            .map(|name| (name.clone(), NoCaseMetadata))
            .collect())
    }

    fn list_ignored_tests(&self) -> Result<Vec<String>> {
        Ok(self.ignored_tests.clone())
    }

    fn build_command(
        &self,
        case_name: &str,
        _case_metadata: &NoCaseMetadata,
    ) -> (Utf8PathBuf, Vec<String>) {
        let binary_name = self.path().file_name().unwrap().to_str().unwrap();
        (format!("/{binary_name}").into(), vec![case_name.into()])
    }

    fn format_case(
        &self,
        package_name: &str,
        case_name: &str,
        _case_metadata: &NoCaseMetadata,
    ) -> String {
        format!("{package_name} {case_name}")
    }
}

#[derive(Clone, Debug)]
pub struct FakeTestPackage {
    pub name: String,
    pub artifacts: Vec<StringArtifactKey>,
    pub id: FakePackageId,
}

impl TestPackage for FakeTestPackage {
    type PackageId = FakePackageId;
    type ArtifactKey = StringArtifactKey;

    fn name(&self) -> &str {
        &self.name
    }

    fn artifacts(&self) -> Vec<Self::ArtifactKey> {
        self.artifacts.clone()
    }

    fn id(&self) -> Self::PackageId {
        self.id.clone()
    }
}

pub struct FakeTestFilter(SimpleFilter);

impl std::str::FromStr for FakeTestFilter {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(Self(SimpleFilter::from_str(s)?))
    }
}

impl TestFilter for FakeTestFilter {
    type Package = FakeTestPackage;
    type ArtifactKey = StringArtifactKey;
    type CaseMetadata = NoCaseMetadata;

    fn compile(include: &[String], exclude: &[String]) -> Result<Self> {
        Ok(Self(SimpleFilter::compile(include, exclude)?))
    }

    fn filter(
        &self,
        package: &FakeTestPackage,
        artifact: Option<&Self::ArtifactKey>,
        case: Option<(&str, &NoCaseMetadata)>,
    ) -> Option<bool> {
        self.0
            .filter(&StringPackage(package.name().into()), artifact, case)
    }
}

pub struct TestOptions;

impl CollectTests for TestCollector {
    const ENQUEUE_MESSAGE: &'static str = "building artifacts...";

    type BuildHandle = WaitForNothing;
    type Artifact = FakeTestArtifact;
    type ArtifactStream = std::vec::IntoIter<Result<FakeTestArtifact>>;
    type Options = TestOptions;
    type TestFilter = FakeTestFilter;
    type ArtifactKey = StringArtifactKey;
    type PackageId = FakePackageId;
    type Package = FakeTestPackage;
    type CaseMetadata = NoCaseMetadata;

    fn start(
        &self,
        _color: bool,
        _options: &TestOptions,
        packages: Vec<&FakeTestPackage>,
        _ui: &ui::UiSender,
    ) -> Result<(Self::BuildHandle, Self::ArtifactStream)> {
        let fs = Fs::new();
        fs.create_dir_all(&self.target_dir).unwrap();
        fs.write((**self.target_dir).join("test_run"), "").unwrap();

        let artifacts: Vec<_> = self.tests.artifacts(&self.bin_path, packages);
        Ok((WaitForNothing, artifacts.into_iter()))
    }

    fn get_test_layers(
        &self,
        _artifact: &FakeTestArtifact,
        _metadata: &TestMetadata,
        _ind: &ui::UiSender,
    ) -> Result<Vec<LayerSpec>> {
        Ok(vec![])
    }

    fn remove_fixture_output(case_str: &str, lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .filter(|line| {
                !(line.starts_with("fixture") || line.starts_with(&format!("{case_str} FAILED")))
            })
            .collect()
    }

    fn get_packages(&self, _ui: &ui::UiSender) -> Result<Vec<FakeTestPackage>> {
        Ok(self.tests.packages())
    }
}
