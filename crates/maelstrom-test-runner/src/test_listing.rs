use crate::{TestArtifactKey, TestCaseMetadata, TestFilter};
use anyhow::{anyhow, bail, Result};
use maelstrom_base::{nonempty, NonEmpty};
use maelstrom_client::StateDir;
use maelstrom_util::{
    fs::Fs,
    root::{Root, RootBuf},
};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use serde_with::{serde_as, DisplayFromStr, DurationSecondsWithFrac};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    marker::PhantomData,
    path::Path,
    time::Duration,
};

/*  _
 * (_)_ __    _ __ ___   ___ _ __ ___   ___  _ __ _   _
 * | | '_ \  | '_ ` _ \ / _ \ '_ ` _ \ / _ \| '__| | | |
 * | | | | | | | | | | |  __/ | | | | | (_) | |  | |_| |
 * |_|_| |_| |_| |_| |_|\___|_| |_| |_|\___/|_|   \__, |
 *                                                |___/
 *  FIGLET: in memory
 */

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseOutcome {
    Success,
    Failure,
}

impl CaseOutcome {
    pub fn from_failure(failure: bool) -> Self {
        if failure {
            Self::Failure
        } else {
            Self::Success
        }
    }

    pub fn combine(self, other: Self) -> Self {
        if let Self::Success = self {
            other
        } else {
            Self::Failure
        }
    }

    pub fn failure(&self) -> bool {
        matches!(self, Self::Failure)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CaseData<CaseMetadataT> {
    metadata: CaseMetadataT,
    when_read: Option<(CaseOutcome, NonEmpty<Duration>)>,
    this_run: Option<(CaseOutcome, NonEmpty<Duration>)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Artifact<CaseMetadataT> {
    cases: HashMap<String, CaseData<CaseMetadataT>>,
}

impl<CaseMetadataT> Default for Artifact<CaseMetadataT> {
    fn default() -> Self {
        Self {
            cases: HashMap::new(),
        }
    }
}

impl<CaseMetadataT: TestCaseMetadata, K: Into<String>> FromIterator<(K, CaseData<CaseMetadataT>)>
    for Artifact<CaseMetadataT>
{
    fn from_iter<T: IntoIterator<Item = (K, CaseData<CaseMetadataT>)>>(iter: T) -> Self {
        Self {
            cases: HashMap::from_iter(iter.into_iter().map(|(k, v)| (k.into(), v))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Package<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata> {
    artifacts: HashMap<ArtifactKeyT, Artifact<CaseMetadataT>>,
}

impl<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata> Default
    for Package<ArtifactKeyT, CaseMetadataT>
{
    fn default() -> Self {
        Self {
            artifacts: HashMap::new(),
        }
    }
}

impl<ArtifactKeyT, CaseMetadataT, K, V> FromIterator<(K, V)>
    for Package<ArtifactKeyT, CaseMetadataT>
where
    ArtifactKeyT: TestArtifactKey,
    CaseMetadataT: TestCaseMetadata,
    K: Into<ArtifactKeyT>,
    V: Into<Artifact<CaseMetadataT>>,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self {
            artifacts: HashMap::from_iter(iter.into_iter().map(|(k, v)| (k.into(), v.into()))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestDb<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata> {
    packages: HashMap<String, Package<ArtifactKeyT, CaseMetadataT>>,
}

impl<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata> Default
    for TestDb<ArtifactKeyT, CaseMetadataT>
{
    fn default() -> Self {
        Self {
            packages: HashMap::new(),
        }
    }
}

impl<ArtifactKeyT, CaseMetadataT, K, V> FromIterator<(K, V)> for TestDb<ArtifactKeyT, CaseMetadataT>
where
    ArtifactKeyT: TestArtifactKey,
    CaseMetadataT: TestCaseMetadata,
    K: Into<String>,
    V: Into<Package<ArtifactKeyT, CaseMetadataT>>,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self {
            packages: HashMap::from_iter(iter.into_iter().map(|(k, v)| (k.into(), v.into()))),
        }
    }
}

impl<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata>
    TestDb<ArtifactKeyT, CaseMetadataT>
{
    pub fn update_artifact_cases<K, I, T>(&mut self, package_name: &str, artifact_key: K, cases: I)
    where
        K: Into<ArtifactKeyT>,
        I: IntoIterator<Item = (T, CaseMetadataT)>,
        T: Into<String>,
    {
        let package = self.packages.entry(package_name.into()).or_default();
        let artifact = package.artifacts.entry(artifact_key.into()).or_default();
        let mut cases: HashMap<String, CaseMetadataT> = cases
            .into_iter()
            .map(|(case_name, metadata)| (case_name.into(), metadata))
            .collect();
        artifact.cases.retain(|case_name, case| {
            if let Some(metadata) = cases.remove(case_name) {
                case.metadata = metadata;
                true
            } else {
                false
            }
        });
        artifact
            .cases
            .extend(cases.into_iter().map(|(case_name, metadata)| {
                (
                    case_name,
                    CaseData {
                        metadata,
                        when_read: None,
                        this_run: None,
                    },
                )
            }));
    }

    pub fn retain_packages_and_artifacts<'a, PI, PN, AI, AK>(&mut self, existing_packages: PI)
    where
        PI: IntoIterator<Item = (PN, AI)>,
        PN: Into<&'a str>,
        AI: IntoIterator<Item = AK>,
        AK: Into<ArtifactKeyT>,
    {
        let existing_packages: HashMap<_, HashSet<_>> = existing_packages
            .into_iter()
            .map(|(pn, ai)| (pn.into(), ai.into_iter().map(Into::into).collect()))
            .collect();
        self.packages.retain(|package_name, package| {
            let Some(existing_artifacts) = existing_packages.get(package_name.as_str()) else {
                return false;
            };
            package
                .artifacts
                .retain(|key, _| existing_artifacts.contains(key));
            true
        });
    }

    pub fn expected_job_count<TestFilterT>(
        &self,
        packages: &BTreeMap<String, TestFilterT::Package>,
        filter: &TestFilterT,
    ) -> u64
    where
        TestFilterT: TestFilter<ArtifactKey = ArtifactKeyT, CaseMetadata = CaseMetadataT>,
    {
        self.packages
            .iter()
            .flat_map(|(p, a)| {
                a.artifacts
                    .iter()
                    .flat_map(move |(a, c)| c.cases.iter().map(move |(c, cd)| (p, a, c, cd)))
            })
            .filter(|(p, a, c, cd)| {
                let Some(p) = packages.get(*p) else {
                    return false;
                };
                filter
                    .filter(p, Some(a), Some((c, &cd.metadata)))
                    .expect("case is provided")
            })
            .count() as u64
    }

    pub fn add_timing(
        &mut self,
        package_name: &str,
        artifact_key: ArtifactKeyT,
        case_name: &str,
        failed: bool,
        timing: Duration,
    ) {
        const MAX_TIMINGS_PER_CASE: usize = 3;
        fn add_timing(timings: &mut NonEmpty<Duration>, timing: Duration) {
            timings.push(timing);
            if timings.len() > MAX_TIMINGS_PER_CASE {
                *timings = NonEmpty::collect(
                    timings
                        .iter()
                        .skip(timings.len() - MAX_TIMINGS_PER_CASE)
                        .copied(),
                )
                .unwrap();
            }
        }
        let package = self.packages.entry(package_name.to_owned()).or_default();
        let artifact = package.artifacts.entry(artifact_key).or_default();
        let case = artifact
            .cases
            .get_mut(case_name)
            .expect("case should have been added");

        let new_outcome = CaseOutcome::from_failure(failed);
        match &mut case.this_run {
            this_run @ None => {
                let timings = match &case.when_read {
                    None => nonempty![timing],
                    Some((_, timings)) => {
                        let mut timings = timings.clone();
                        add_timing(&mut timings, timing);
                        timings
                    }
                };
                this_run.replace((new_outcome, timings));
            }
            Some((outcome, timings)) => {
                add_timing(timings, timing);
                *outcome = outcome.combine(new_outcome);
            }
        };
    }

    /// Return some information about the specified test case.
    ///
    /// If the returned value is `Option::None`, it means that the test runner doesn't have a
    /// record of ever running the test case.
    ///
    /// If the returned value is `Option::Some`, then the `SuccessOrFailure` field indicates
    /// whether or not there were any failures the last time the test runner ran the test case. If
    /// it is `SuccessOrFailure::Success`, it means that all test cases that were run the last
    /// time, assuming it was run at least once, were successful. Otherwise, it means that at least
    /// one test case was a failure. Note that a test case can be run multiple times by a test
    /// runner with the `repeat` flag.
    pub fn get_timing(
        &self,
        package_name: &str,
        artifact_key: &ArtifactKeyT,
        case_name: &str,
    ) -> Option<(CaseOutcome, Duration)> {
        self.packages
            .get(package_name)?
            .artifacts
            .get(artifact_key)?
            .cases
            .get(case_name)?
            .when_read
            .as_ref()
            .map(|(outcome, timings)| {
                let len: u32 = timings.len().try_into().unwrap();
                (
                    *outcome,
                    timings
                        .iter()
                        .fold(Duration::ZERO, |avg, timing| avg + *timing / len),
                )
            })
    }
}

/*                    _ _     _
 *   ___  _ __     __| (_)___| | __
 *  / _ \| '_ \   / _` | / __| |/ /
 * | (_) | | | | | (_| | \__ \   <
 *  \___/|_| |_|  \__,_|_|___/_|\_\
 *  FIGLET: on disk
 */

#[derive(Deserialize_repr, Eq, FromPrimitive, PartialEq, Serialize_repr)]
#[repr(u32)]
enum OnDiskTestDbVersion {
    V3 = 3,
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum OnDiskCaseOutcome {
    #[default]
    Success,
    Failure,
    New,
}

impl OnDiskCaseOutcome {
    fn success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

impl From<&CaseOutcome> for OnDiskCaseOutcome {
    fn from(outcome: &CaseOutcome) -> Self {
        match outcome {
            CaseOutcome::Success => Self::Success,
            CaseOutcome::Failure => Self::Failure,
        }
    }
}

#[serde_as]
#[derive(Clone, Serialize, Deserialize)]
struct OnDiskCaseData<CaseMetadataT: TestCaseMetadata> {
    #[serde_as(as = "Vec<DurationSecondsWithFrac>")]
    timings: Vec<Duration>,
    #[serde(bound(serialize = ""))]
    #[serde(bound(deserialize = ""))]
    #[serde(flatten)]
    metadata: CaseMetadataT,
    #[serde(default)]
    outcome: OnDiskCaseOutcome,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
struct OnDiskArtifact<CaseMetadataT: TestCaseMetadata> {
    #[serde(bound(serialize = ""))]
    #[serde(bound(deserialize = ""))]
    cases: BTreeMap<String, OnDiskCaseData<CaseMetadataT>>,
}

#[serde_as]
#[derive(Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
struct OnDiskArtifactKey<ArtifactKeyT: TestArtifactKey> {
    #[serde_as(as = "DisplayFromStr")]
    #[serde(bound(serialize = ""))]
    #[serde(bound(deserialize = ""))]
    key: ArtifactKeyT,
}

#[serde_as]
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct OnDiskPackage<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata> {
    #[serde(bound(serialize = ""))]
    #[serde(bound(deserialize = ""))]
    artifacts: BTreeMap<OnDiskArtifactKey<ArtifactKeyT>, OnDiskArtifact<CaseMetadataT>>,
}

#[derive(Serialize, Deserialize)]
struct OnDiskTestDb<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata> {
    version: OnDiskTestDbVersion,
    #[serde(flatten)]
    #[serde(bound(serialize = ""))]
    #[serde(bound(deserialize = ""))]
    packages: BTreeMap<String, OnDiskPackage<ArtifactKeyT, CaseMetadataT>>,
}

impl<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata>
    From<TestDb<ArtifactKeyT, CaseMetadataT>> for OnDiskTestDb<ArtifactKeyT, CaseMetadataT>
{
    fn from(in_memory: TestDb<ArtifactKeyT, CaseMetadataT>) -> Self {
        Self {
            version: OnDiskTestDbVersion::V3,
            packages: in_memory
                .packages
                .into_iter()
                .map(|(package_name, package)| {
                    (
                        package_name,
                        OnDiskPackage {
                            artifacts: package
                                .artifacts
                                .into_iter()
                                .map(|(key, artifact)| {
                                    (
                                        OnDiskArtifactKey { key },
                                        OnDiskArtifact {
                                            cases: {
                                                let mut cases =
                                                    Vec::from_iter(artifact.cases.into_iter().map(
                                                        |(case, data)| {
                                                            (case, {
                                                                if let Some((outcome, timings)) =
                                                                    &data.this_run
                                                                {
                                                                    OnDiskCaseData {
                                                                        timings: timings
                                                                            .iter()
                                                                            .cloned()
                                                                            .collect(),
                                                                        metadata: data.metadata,
                                                                        outcome: outcome.into(),
                                                                    }
                                                                } else if let Some((
                                                                    outcome,
                                                                    timings,
                                                                )) = &data.when_read
                                                                {
                                                                    OnDiskCaseData {
                                                                        timings: timings
                                                                            .iter()
                                                                            .cloned()
                                                                            .collect(),
                                                                        metadata: data.metadata,
                                                                        outcome: outcome.into(),
                                                                    }
                                                                } else {
                                                                    OnDiskCaseData {
                                                                        timings: vec![],
                                                                        metadata: data.metadata,
                                                                        outcome:
                                                                            OnDiskCaseOutcome::New,
                                                                    }
                                                                }
                                                            })
                                                        },
                                                    ));
                                                cases.sort_by(|(name1, _), (name2, _)| {
                                                    name1.cmp(name2)
                                                });
                                                cases.into_iter().collect()
                                            },
                                        },
                                    )
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
        }
    }
}

impl<ArtifactKeyT: TestArtifactKey, CaseMetadataT: TestCaseMetadata>
    From<OnDiskTestDb<ArtifactKeyT, CaseMetadataT>> for TestDb<ArtifactKeyT, CaseMetadataT>
{
    fn from(on_disk: OnDiskTestDb<ArtifactKeyT, CaseMetadataT>) -> Self {
        Self::from_iter(on_disk.packages.into_iter().map(|(package_name, package)| {
            (
                package_name,
                Package::from_iter(package.artifacts.into_iter().map(|(key, artifact)| {
                    (
                        key.key,
                        Artifact::from_iter(artifact.cases.into_iter().map(|(case, data)| {
                            let timings = NonEmpty::collect(data.timings.iter().copied());
                            let when_read = timings.map(|timings| {
                                // We should never have an outcome of "new", but if we manage to
                                // get it, just turn it into "failure".
                                (CaseOutcome::from_failure(!data.outcome.success()), timings)
                            });
                            (
                                case,
                                CaseData {
                                    metadata: data.metadata,
                                    when_read,
                                    this_run: None,
                                },
                            )
                        })),
                    )
                })),
            )
        }))
    }
}

/*      _
 *  ___| |_ ___  _ __ ___
 * / __| __/ _ \| '__/ _ \
 * \__ \ || (_) | | |  __/
 * |___/\__\___/|_|  \___|
 *  FIGLET: store
 */

pub trait TestDbStoreDeps {
    fn read_to_string_if_exists(&self, path: impl AsRef<Path>) -> Result<Option<String>> {
        unimplemented!("{:?}", path.as_ref());
    }
    fn create_dir_all(&self, path: impl AsRef<Path>) -> Result<()> {
        unimplemented!("{:?}", path.as_ref());
    }
    fn write(&self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
        unimplemented!("{:?} {:?}", path.as_ref(), contents.as_ref());
    }
}

impl TestDbStoreDeps for Fs {
    fn read_to_string_if_exists(&self, path: impl AsRef<Path>) -> Result<Option<String>> {
        Fs::read_to_string_if_exists(self, path)
    }

    fn create_dir_all(&self, path: impl AsRef<Path>) -> Result<()> {
        Fs::create_dir_all(self, path)
    }

    fn write(&self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
        Fs::write(self, path, contents)
    }
}

struct TestDbFile;

pub struct TestDbStore<ArtifactKeyT, CaseMetadataT, DepsT = Fs> {
    generics: PhantomData<(ArtifactKeyT, CaseMetadataT)>,
    deps: DepsT,
    test_listing_file: RootBuf<TestDbFile>,
}

const MISSING_VERSION: &str = "missing version";
const VERSION_NOT_AN_INTEGER: &str = "version field is not an integer";
const TEST_DB_FILE: &str = "test-listing.toml";

impl<ArtifactKeyT, CaseMetadataT, DepsT> TestDbStore<ArtifactKeyT, CaseMetadataT, DepsT> {
    pub fn new(deps: DepsT, state_dir: impl AsRef<Root<StateDir>>) -> Self {
        Self {
            generics: PhantomData,
            deps,
            test_listing_file: state_dir.as_ref().join(TEST_DB_FILE),
        }
    }
}

impl<ArtifactKeyT, CaseMetadataT, DepsT> TestDbStore<ArtifactKeyT, CaseMetadataT, DepsT>
where
    ArtifactKeyT: TestArtifactKey,
    CaseMetadataT: TestCaseMetadata,
    DepsT: TestDbStoreDeps,
{
    pub fn load(&self) -> Result<TestDb<ArtifactKeyT, CaseMetadataT>> {
        let Some(contents) = self
            .deps
            .read_to_string_if_exists(&self.test_listing_file)?
        else {
            return Ok(Default::default());
        };
        let mut table: toml::Table = toml::from_str(&contents)?;
        let version = table
            .remove("version")
            .ok_or_else(|| anyhow!(MISSING_VERSION))?;
        let Some(version) = version.as_integer() else {
            bail!(VERSION_NOT_AN_INTEGER);
        };
        match OnDiskTestDbVersion::from_i64(version) {
            None => Ok(Default::default()),
            Some(OnDiskTestDbVersion::V3) => {
                Ok(toml::from_str::<OnDiskTestDb<ArtifactKeyT, CaseMetadataT>>(&contents)?.into())
            }
        }
    }

    pub fn save(&self, job_listing: TestDb<ArtifactKeyT, CaseMetadataT>) -> Result<()> {
        self.deps
            .create_dir_all(self.test_listing_file.parent().unwrap())?;
        self.deps.write(
            &self.test_listing_file,
            toml::to_string::<OnDiskTestDb<ArtifactKeyT, CaseMetadataT>>(&job_listing.into())?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NoCaseMetadata, SimpleFilter, StringArtifactKey};
    use indoc::indoc;
    use maelstrom_base::nonempty;
    use maelstrom_test::millis;
    use maelstrom_util::ext::OptionExt as _;
    use pretty_assertions::assert_eq;
    use std::{cell::RefCell, rc::Rc, str};
    use CaseOutcome::{Failure, Success};

    macro_rules! millis {
        ($millis:expr) => {
            Duration::from_millis($millis)
        };
    }

    fn artifact_from_cases_with_metadata<MetadataT: TestCaseMetadata>(
        iter: impl IntoIterator<
            Item = (
                &'static str,
                MetadataT,
                Option<(CaseOutcome, NonEmpty<Duration>)>,
                Option<(CaseOutcome, NonEmpty<Duration>)>,
            ),
        >,
    ) -> Artifact<MetadataT> {
        Artifact::from_iter(
            iter.into_iter()
                .map(|(name, metadata, when_read, this_run)| {
                    (
                        name,
                        CaseData {
                            metadata,
                            when_read,
                            this_run,
                        },
                    )
                }),
        )
    }

    fn artifact_from_cases(
        iter: impl IntoIterator<
            Item = (
                &'static str,
                Option<(CaseOutcome, NonEmpty<Duration>)>,
                Option<(CaseOutcome, NonEmpty<Duration>)>,
            ),
        >,
    ) -> Artifact<NoCaseMetadata> {
        artifact_from_cases_with_metadata(
            iter.into_iter()
                .map(|(name, when_read, this_run)| (name, NoCaseMetadata, when_read, this_run)),
        )
    }

    #[test]
    fn update_artifact_cases() {
        let mut listing = TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
            "package-1",
            Package::from_iter([(
                StringArtifactKey::from("artifact-1.library"),
                artifact_from_cases([
                    (
                        "case-1-1L-1",
                        Some((Success, nonempty![millis!(10), millis!(11), millis!(12)])),
                        None,
                    ),
                    (
                        "case-1-1L-2",
                        Some((Failure, nonempty![millis!(20), millis!(21)])),
                        None,
                    ),
                    ("case-1-1L-3", None, Some((Failure, nonempty![millis!(30)]))),
                ]),
            )]),
        )]);

        // Add some more cases with the same artifact name, but a different kind, in the same
        // package.
        listing.update_artifact_cases(
            "package-1",
            StringArtifactKey::from("artifact-1.binary"),
            [
                ("case-1-1B-1", NoCaseMetadata),
                ("case-1-1B-2", NoCaseMetadata),
                ("case-1-1B-3", NoCaseMetadata),
            ],
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([
                    (
                        StringArtifactKey::from("artifact-1.library"),
                        artifact_from_cases([
                            (
                                "case-1-1L-1",
                                Some((Success, nonempty![millis!(10), millis!(11), millis!(12)])),
                                None,
                            ),
                            (
                                "case-1-1L-2",
                                Some((Failure, nonempty![millis!(20), millis!(21)])),
                                None,
                            ),
                            ("case-1-1L-3", None, Some((Failure, nonempty![millis!(30)]))),
                        ]),
                    ),
                    (
                        StringArtifactKey::from("artifact-1.binary"),
                        artifact_from_cases([
                            ("case-1-1B-1", None, None),
                            ("case-1-1B-2", None, None),
                            ("case-1-1B-3", None, None),
                        ]),
                    ),
                ]),
            )]),
        );

        // Add some more cases that partially overlap with previous ones. This should retain the
        // timings, but remove cases that no longer exist.
        listing.update_artifact_cases(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            [
                ("case-1-1L-2", NoCaseMetadata),
                ("case-1-1L-3", NoCaseMetadata),
                ("case-1-1L-4", NoCaseMetadata),
            ],
        );
        listing.update_artifact_cases(
            "package-1",
            StringArtifactKey::from("artifact-1.binary"),
            [
                ("case-1-1B-2", NoCaseMetadata),
                ("case-1-1B-3", NoCaseMetadata),
                ("case-1-1B-4", NoCaseMetadata),
            ],
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([
                    (
                        StringArtifactKey::from("artifact-1.library"),
                        artifact_from_cases([
                            (
                                "case-1-1L-2",
                                Some((Failure, nonempty![millis!(20), millis!(21)])),
                                None,
                            ),
                            ("case-1-1L-3", None, Some((Failure, nonempty![millis!(30)]))),
                            ("case-1-1L-4", None, None),
                        ]),
                    ),
                    (
                        StringArtifactKey::from("artifact-1.binary"),
                        artifact_from_cases([
                            ("case-1-1B-2", None, None),
                            ("case-1-1B-3", None, None),
                            ("case-1-1B-4", None, None),
                        ]),
                    ),
                ]),
            )]),
        );

        // Add some more cases for a different package. They should be independent.
        listing.update_artifact_cases(
            "package-2",
            StringArtifactKey::from("artifact-1.library"),
            [
                ("case-2-1L-1", NoCaseMetadata),
                ("case-2-1L-2", NoCaseMetadata),
                ("case-2-1L-3", NoCaseMetadata),
            ],
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([
                (
                    "package-1",
                    Package::from_iter([
                        (
                            StringArtifactKey::from("artifact-1.library"),
                            artifact_from_cases([
                                (
                                    "case-1-1L-2",
                                    Some((Failure, nonempty![millis!(20), millis!(21)])),
                                    None,
                                ),
                                ("case-1-1L-3", None, Some((Failure, nonempty![millis!(30)]))),
                                ("case-1-1L-4", None, None),
                            ]),
                        ),
                        (
                            StringArtifactKey::from("artifact-1.binary"),
                            artifact_from_cases([
                                ("case-1-1B-2", None, None),
                                ("case-1-1B-3", None, None),
                                ("case-1-1B-4", None, None),
                            ]),
                        ),
                    ])
                ),
                (
                    "package-2",
                    Package::from_iter([(
                        StringArtifactKey::from("artifact-1.library"),
                        artifact_from_cases([
                            ("case-2-1L-1", None, None),
                            ("case-2-1L-2", None, None),
                            ("case-2-1L-3", None, None),
                        ]),
                    )]),
                ),
            ]),
        );
    }

    #[test]
    fn update_artifact_cases_with_changed_metadata() {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        enum FakeCaseMetadata {
            A,
            B,
        }

        impl TestCaseMetadata for FakeCaseMetadata {}

        let mut listing = TestDb::<StringArtifactKey, FakeCaseMetadata>::from_iter([(
            "package-1",
            Package::from_iter([(
                StringArtifactKey::from("artifact-1.library"),
                artifact_from_cases_with_metadata([
                    (
                        "case-1-1L-1",
                        FakeCaseMetadata::A,
                        Some((Success, nonempty![millis!(10), millis!(11), millis!(12)])),
                        None,
                    ),
                    (
                        "case-1-1L-2",
                        FakeCaseMetadata::A,
                        Some((Failure, nonempty![millis!(20), millis!(21)])),
                        None,
                    ),
                    (
                        "case-1-1L-3",
                        FakeCaseMetadata::A,
                        None,
                        Some((Failure, nonempty![millis!(30)])),
                    ),
                ]),
            )]),
        )]);

        // Add some more cases that partially overlap with previous ones. This should retain the
        // timings, but remove cases that no longer exist.
        listing.update_artifact_cases(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            [
                ("case-1-1L-2", FakeCaseMetadata::B),
                ("case-1-1L-3", FakeCaseMetadata::B),
                ("case-1-1L-4", FakeCaseMetadata::B),
            ],
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, FakeCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases_with_metadata([
                        (
                            "case-1-1L-2",
                            FakeCaseMetadata::B,
                            Some((Failure, nonempty![millis!(20), millis!(21)])),
                            None,
                        ),
                        (
                            "case-1-1L-3",
                            FakeCaseMetadata::B,
                            None,
                            Some((Failure, nonempty![millis!(30)]))
                        ),
                        ("case-1-1L-4", FakeCaseMetadata::B, None, None),
                    ]),
                ),]),
            )]),
        );
    }

    #[test]
    fn retain_packages_and_artifacts() {
        let mut listing = TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([
            (
                "package-1",
                Package::from_iter([
                    (
                        StringArtifactKey::from("artifact-1.library"),
                        artifact_from_cases([
                            (
                                "case-1-1L-1",
                                Some((Failure, nonempty![millis!(10), millis!(11)])),
                                None,
                            ),
                            (
                                "case-1-1L-2",
                                Some((Success, nonempty![millis!(20)])),
                                Some((Failure, nonempty![millis!(20), millis!(21)])),
                            ),
                        ]),
                    ),
                    (
                        StringArtifactKey::from("artifact-1.binary"),
                        artifact_from_cases([
                            (
                                "case-1-1B-1",
                                Some((Failure, nonempty![millis!(15), millis!(16)])),
                                None,
                            ),
                            (
                                "case-1-1B-2",
                                Some((Success, nonempty![millis!(25)])),
                                Some((Success, nonempty![millis!(25), millis!(26)])),
                            ),
                        ]),
                    ),
                ]),
            ),
            (
                "package-2",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([
                        (
                            "case-2-1L-1",
                            Some((Success, nonempty![millis!(10), millis!(12)])),
                            None,
                        ),
                        (
                            "case-2-1L-2",
                            Some((Failure, nonempty![millis!(20), millis!(22)])),
                            None,
                        ),
                    ]),
                )]),
            ),
        ]);

        listing.retain_packages_and_artifacts([
            (
                "package-1",
                vec![
                    StringArtifactKey::from("artifact-1.library"),
                    StringArtifactKey::from("artifact-2.binary"),
                ],
            ),
            (
                "package-3",
                vec![StringArtifactKey::from("artifact-1.library")],
            ),
        ]);

        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([
                        (
                            "case-1-1L-1",
                            Some((Failure, nonempty![millis!(10), millis!(11)])),
                            None
                        ),
                        (
                            "case-1-1L-2",
                            Some((Success, nonempty![millis!(20)])),
                            Some((Failure, nonempty![millis!(20), millis!(21)])),
                        ),
                    ]),
                )]),
            )]),
        );
    }

    #[test]
    fn expected_job_count() {
        let listing = TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([
            (
                "package-1",
                Package::from_iter([
                    (
                        StringArtifactKey::from("artifact-1.library"),
                        artifact_from_cases([
                            (
                                "case-1-1L-1",
                                Some((Success, nonempty![millis!(10), millis!(11)])),
                                None,
                            ),
                            ("case-1-1L-2", Some((Success, nonempty![millis!(20)])), None),
                            ("case-1-1L-3", None, None),
                        ]),
                    ),
                    (
                        StringArtifactKey::from("artifact-1.binary"),
                        artifact_from_cases([
                            (
                                "case-1-1B-1",
                                Some((Success, nonempty![millis!(15), millis!(16)])),
                                None,
                            ),
                            ("case-1-1B-2", None, None),
                        ]),
                    ),
                ]),
            ),
            (
                "package-2",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([(
                        "case-2-1L-1",
                        Some((Success, nonempty![millis!(10), millis!(12)])),
                        None,
                    )]),
                )]),
            ),
        ]);

        let packages = ["package-1", "package-2"]
            .into_iter()
            .map(|p| (p.into(), p.into()))
            .collect();

        assert_eq!(listing.expected_job_count(&packages, &SimpleFilter::All), 6);
        assert_eq!(
            listing.expected_job_count(&packages, &SimpleFilter::None),
            0
        );
        assert_eq!(
            listing.expected_job_count(&packages, &SimpleFilter::Package("package-1".into())),
            5
        );
        assert_eq!(
            listing.expected_job_count(
                &packages,
                &SimpleFilter::ArtifactEndsWith(".library".into())
            ),
            4
        );
    }

    #[test]
    fn add_timing_none_before() {
        let mut listing = TestDb::default();

        listing.update_artifact_cases(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            [("case-1-1L-1", NoCaseMetadata)],
        );

        listing.add_timing(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            "case-1-1L-1",
            false,
            millis!(10),
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([(
                        "case-1-1L-1",
                        None,
                        Some((Success, nonempty![millis!(10)]))
                    )]),
                )]),
            )]),
        );

        listing.add_timing(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            "case-1-1L-1",
            true,
            millis!(11),
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([(
                        "case-1-1L-1",
                        None,
                        Some((Failure, nonempty![millis!(10), millis!(11)]))
                    )]),
                )]),
            )]),
        );

        listing.add_timing(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            "case-1-1L-1",
            false,
            millis!(12),
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([(
                        "case-1-1L-1",
                        None,
                        Some((Failure, nonempty![millis!(10), millis!(11), millis!(12)]))
                    )]),
                )]),
            )]),
        );

        listing.add_timing(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            "case-1-1L-1",
            false,
            millis!(13),
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([(
                        "case-1-1L-1",
                        None,
                        Some((Failure, nonempty![millis!(11), millis!(12), millis!(13)]))
                    )]),
                )]),
            )]),
        );
    }

    #[test]
    fn add_timing_already_too_many() {
        let mut listing = TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
            "package-1",
            Package::from_iter([(
                StringArtifactKey::from("artifact-1.library"),
                artifact_from_cases([(
                    "case-1-1L-1",
                    Some((
                        Failure,
                        nonempty![
                            millis!(10),
                            millis!(11),
                            millis!(12),
                            millis!(13),
                            millis!(14)
                        ],
                    )),
                    None,
                )]),
            )]),
        )]);

        listing.add_timing(
            "package-1",
            StringArtifactKey::from("artifact-1.library"),
            "case-1-1L-1",
            false,
            millis!(15),
        );
        assert_eq!(
            listing,
            TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
                "package-1",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([(
                        "case-1-1L-1",
                        Some((
                            Failure,
                            nonempty![
                                millis!(10),
                                millis!(11),
                                millis!(12),
                                millis!(13),
                                millis!(14)
                            ]
                        )),
                        Some((Success, nonempty![millis!(13), millis!(14), millis!(15)])),
                    )]),
                )]),
            )]),
        );
    }

    #[test]
    fn get_timing() {
        let artifact_1 = StringArtifactKey::from("artifact-1.library");
        let listing = TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
            "package-1",
            Package::from_iter([(
                artifact_1.clone(),
                artifact_from_cases([
                    ("case-1", None, None),
                    ("case-2", Some((Failure, nonempty![millis!(10)])), None),
                    (
                        "case-3",
                        Some((Success, nonempty![millis!(10), millis!(12)])),
                        None,
                    ),
                    (
                        "case-4",
                        None,
                        Some((Success, nonempty![millis!(10), millis!(12)])),
                    ),
                ]),
            )]),
        )]);

        assert_eq!(listing.get_timing("package-1", &artifact_1, "case-1"), None);
        assert_eq!(
            listing.get_timing("package-1", &artifact_1, "case-2"),
            Some((Failure, millis!(10))),
        );
        assert_eq!(
            listing.get_timing("package-1", &artifact_1, "case-3"),
            Some((Success, millis!(11))),
        );
        assert_eq!(listing.get_timing("package-1", &artifact_1, "case-4"), None);
        assert_eq!(listing.get_timing("package-1", &artifact_1, "case-5"), None);
        let artifact_1_bin = StringArtifactKey::from("artifact-1.binary");
        assert_eq!(
            listing.get_timing("package-1", &artifact_1_bin, "case-1"),
            None,
        );
        assert_eq!(listing.get_timing("package-2", &artifact_1, "case-1"), None);
    }

    #[test]
    fn load_passes_proper_path() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, path: impl AsRef<Path>) -> Result<Option<String>> {
                assert_eq!(
                    path.as_ref().to_str().unwrap(),
                    format!("path/to/state/{TEST_DB_FILE}")
                );
                Ok(None)
            }
        }
        let _ = TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(
            Deps,
            RootBuf::new("path/to/state".into()),
        );
    }

    #[test]
    fn error_reading_in_load_propagates_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Err(anyhow!("error!"))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        assert_eq!(store.load().unwrap_err().to_string(), "error!");
    }

    #[test]
    fn load_of_nonexistent_file_gives_default_listing() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(None)
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        assert_eq!(store.load().unwrap(), TestDb::default());
    }

    #[test]
    fn load_of_file_with_invalid_toml_gives_toml_parse_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some(r#""garbage": { "foo", "bar" }"#.into()))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        let error = store.load().unwrap_err().to_string();
        assert!(error.starts_with("TOML parse error"));
    }

    #[test]
    fn load_of_empty_file_gives_missing_version_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some("foo = 3\n".into()))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        assert_eq!(store.load().unwrap_err().to_string(), MISSING_VERSION);
    }

    #[test]
    fn load_of_file_without_version_gives_missing_version_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some("foo = 3\n".into()))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        assert_eq!(store.load().unwrap_err().to_string(), MISSING_VERSION);
    }

    #[test]
    fn load_of_file_with_non_integer_version_gives_version_not_an_integer_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some("version = \"v1\"\n".into()))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        assert_eq!(
            store.load().unwrap_err().to_string(),
            VERSION_NOT_AN_INTEGER
        );
    }

    #[test]
    fn load_of_file_with_old_version_gives_default_listing() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some("version = 0\nfoo = \"bar\"\n".into()))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        assert_eq!(store.load().unwrap(), TestDb::default());
    }

    #[test]
    fn load_of_file_with_newer_version_gives_default_listing() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some("version = 1000000\nfoo = \"bar\"\n".into()))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        assert_eq!(store.load().unwrap(), TestDb::default());
    }

    #[test]
    fn load_of_file_with_correct_version_gives_deserialized_listing() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some(
                    indoc! {r#"
                        version = 3

                        [package-1."artifact-1.library".case-1-1L-1]
                        outcome = "success"
                        timings = [0.01, 0.011]

                        [package-1."artifact-1.library".case-1-1L-2]
                        outcome = "failure"
                        timings = [0.02]

                        [package-1."artifact-1.library".case-1-1L-3]
                        timings = [0.03]

                        [package-1."artifact-1.library".case-1-1L-4]
                        outcome = "success"
                        timings = []

                        [package-1."artifact-1.library".case-1-1L-5]
                        outcome = "failure"
                        timings = []

                        [package-1."artifact-1.library".case-1-1L-6]
                        outcome = "new"
                        timings = []

                        [package-1."artifact-1.library".case-1-1L-7]
                        timings = []
                    "#}
                    .into(),
                ))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        let expected = TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([(
            "package-1",
            Package::from_iter([(
                StringArtifactKey::from("artifact-1.library"),
                artifact_from_cases([
                    (
                        "case-1-1L-1",
                        Some((Success, nonempty![millis!(10), millis!(11)])),
                        None,
                    ),
                    ("case-1-1L-2", Some((Failure, nonempty![millis!(20)])), None),
                    ("case-1-1L-3", Some((Success, nonempty![millis!(30)])), None),
                    ("case-1-1L-4", None, None),
                    ("case-1-1L-5", None, None),
                    ("case-1-1L-6", None, None),
                    ("case-1-1L-7", None, None),
                ]),
            )]),
        )]);
        assert_eq!(store.load().unwrap(), expected);
    }

    #[test]
    fn load_of_file_with_correct_version_but_bad_toml_gives_toml_parse_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn read_to_string_if_exists(&self, _: impl AsRef<Path>) -> Result<Option<String>> {
                Ok(Some(
                    indoc! {r#"
                        version = 3

                        [[frob.blah]]
                        foo = "package1"
                        bar = "Library"
                        baz = [
                            "case1",
                            "case2",
                        ]
                    "#}
                    .into(),
                ))
            }
        }
        let store =
            TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(Deps, RootBuf::new("".into()));
        let error = store.load().unwrap_err().to_string();
        assert!(error.starts_with("TOML parse error"));
    }

    #[test]
    fn error_creating_dir_in_save_propagates_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn create_dir_all(&self, _: impl AsRef<Path>) -> Result<()> {
                Err(anyhow!("error!"))
            }
        }
        let store = TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(
            Deps,
            RootBuf::new("state".into()),
        );
        assert_eq!(
            store.save(TestDb::default()).unwrap_err().to_string(),
            "error!"
        );
    }

    #[test]
    fn error_writing_in_save_propagates_error() {
        struct Deps;
        impl TestDbStoreDeps for Deps {
            fn create_dir_all(&self, _: impl AsRef<Path>) -> Result<()> {
                Ok(())
            }
            fn write(&self, _: impl AsRef<Path>, _: impl AsRef<[u8]>) -> Result<()> {
                Err(anyhow!("error!"))
            }
        }
        let store = TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(
            Deps,
            RootBuf::new("state".into()),
        );
        assert_eq!(
            store.save(TestDb::default()).unwrap_err().to_string(),
            "error!"
        );
    }

    #[derive(Default)]
    struct LoggingDeps {
        create_dir_all: Option<String>,
        write: Option<(String, String)>,
    }

    impl TestDbStoreDeps for Rc<RefCell<LoggingDeps>> {
        fn create_dir_all(&self, path: impl AsRef<Path>) -> Result<()> {
            self.borrow_mut()
                .create_dir_all
                .replace(path.as_ref().to_str().unwrap().to_string())
                .assert_is_none();
            Ok(())
        }
        fn write(&self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
            self.borrow_mut()
                .write
                .replace((
                    path.as_ref().to_str().unwrap().to_string(),
                    str::from_utf8(contents.as_ref()).unwrap().to_string(),
                ))
                .assert_is_none();
            Ok(())
        }
    }

    #[test]
    fn save_creates_parent_directory() {
        let deps = Rc::new(RefCell::new(LoggingDeps::default()));
        let store = TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(
            deps.clone(),
            RootBuf::new("maelstrom/state/".into()),
        );
        store.save(TestDb::default()).unwrap();
        assert_eq!(deps.borrow().create_dir_all, Some("maelstrom/state".into()));
    }

    #[test]
    fn save_of_default() {
        let deps = Rc::new(RefCell::new(LoggingDeps::default()));
        let store = TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(
            deps.clone(),
            RootBuf::new("maelstrom/state/".into()),
        );
        store.save(TestDb::default()).unwrap();
        assert_eq!(
            deps.borrow().write,
            Some((
                format!("maelstrom/state/{TEST_DB_FILE}"),
                "version = 3\n".into()
            ))
        );
    }

    #[test]
    fn save_of_listing() {
        let deps = Rc::new(RefCell::new(LoggingDeps::default()));
        let store = TestDbStore::<StringArtifactKey, NoCaseMetadata, _>::new(
            deps.clone(),
            RootBuf::new("maelstrom/state/".into()),
        );
        let listing = TestDb::<StringArtifactKey, NoCaseMetadata>::from_iter([
            (
                "package-2",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.library"),
                    artifact_from_cases([("case-2-1L-1", None, None)]),
                )]),
            ),
            (
                "package-1",
                Package::from_iter([
                    (
                        StringArtifactKey::from("artifact-1.binary"),
                        artifact_from_cases([
                            (
                                "case-1-1B-1",
                                Some((Success, nonempty![millis!(13), millis!(14)])),
                                None,
                            ),
                            (
                                "case-1-1B-2",
                                Some((Failure, nonempty![millis!(15), millis!(16)])),
                                None,
                            ),
                        ]),
                    ),
                    (
                        StringArtifactKey::from("artifact-1.library"),
                        artifact_from_cases([
                            (
                                "case-1-1L-1",
                                None,
                                Some((Success, nonempty![millis!(30), millis!(40)])),
                            ),
                            (
                                "case-1-1L-2",
                                None,
                                Some((Failure, nonempty![millis!(25), millis!(26)])),
                            ),
                        ]),
                    ),
                ]),
            ),
            (
                "package-3",
                Package::from_iter([(
                    StringArtifactKey::from("artifact-1.binary"),
                    artifact_from_cases([
                        (
                            "case-3-1B-1",
                            Some((Success, nonempty![millis!(13), millis!(14)])),
                            Some((Failure, nonempty![millis!(23), millis!(24)])),
                        ),
                        (
                            "case-3-1B-2",
                            Some((Failure, nonempty![millis!(15), millis!(16)])),
                            Some((Success, nonempty![millis!(25), millis!(26)])),
                        ),
                    ]),
                )]),
            ),
        ]);
        store.save(listing).unwrap();
        let (actual_path, actual_contents) = deps.borrow_mut().write.take().unwrap();
        assert_eq!(actual_path, format!("maelstrom/state/{TEST_DB_FILE}"));
        assert_eq!(
            actual_contents,
            indoc! {r#"
                    version = 3

                    [package-1."artifact-1.binary".case-1-1B-1]
                    timings = [0.013, 0.014]
                    outcome = "success"

                    [package-1."artifact-1.binary".case-1-1B-2]
                    timings = [0.015, 0.016]
                    outcome = "failure"

                    [package-1."artifact-1.library".case-1-1L-1]
                    timings = [0.03, 0.04]
                    outcome = "success"

                    [package-1."artifact-1.library".case-1-1L-2]
                    timings = [0.025, 0.026]
                    outcome = "failure"

                    [package-2."artifact-1.library".case-2-1L-1]
                    timings = []
                    outcome = "new"

                    [package-3."artifact-1.binary".case-3-1B-1]
                    timings = [0.023, 0.024]
                    outcome = "failure"

                    [package-3."artifact-1.binary".case-3-1B-2]
                    timings = [0.025, 0.026]
                    outcome = "success"
                "#},
        );
    }
}
