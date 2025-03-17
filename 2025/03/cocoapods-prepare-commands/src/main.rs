use std::{
    borrow::Cow,
    collections::BTreeMap,
    fmt::Display,
    fs::{self, File, ReadDir},
    iter::zip,
    ops::Deref,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use duct::cmd;
use git2::{
    Commit, Delta, DiffOptions, ObjectType, Oid, Repository, Tree, TreeEntry, TreeWalkResult,
};
use indicatif::ParallelProgressIterator;
use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct Podspec<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
    version: Cow<'a, str>,
    prepare_command: Option<Cow<'a, str>>,

    #[serde(skip)]
    published: DateTime<Utc>,

    #[serde(skip_deserializing)]
    loaded_from: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Res {
    Podspec(Podspec<'static>),
    Error { error: String, path: String },
    NoPrepareCommand,
}

impl Podspec<'_> {
    fn into_owned(self) -> Podspec<'static> {
        Podspec {
            name: self.name.into_owned().into(),
            version: self.version.into_owned().into(),
            prepare_command: self.prepare_command.map(|s| s.into_owned().into()),
            published: self.published,
            loaded_from: self.loaded_from,
        }
    }
}

#[derive(Debug, Serialize)]
struct IterResult {
    commit: String,
    podspecs: BTreeMap<String, Vec<Res>>,
}

fn iter_repo(repo: &str) -> anyhow::Result<IterResult> {
    let repository = Repository::open(repo)?;

    let mut remote = repository.find_remote("origin")?;
    remote.fetch(&["master"], None, None)?;
    let branch = repository.find_branch("origin/master", git2::BranchType::Remote)?;
    let commit = branch.get().peel_to_commit()?;
    println!("Commit: {}", commit.id());

    let tree = commit.tree()?;
    let mut podspecs: BTreeMap<String, Vec<Res>> = BTreeMap::new();
    tree.walk(git2::TreeWalkMode::PostOrder, |s, entry| {
        if entry.kind() != Some(git2::ObjectType::Blob) {
            return TreeWalkResult::Ok;
        }
        if !entry.name_bytes().ends_with(b".podspec.json") {
            return TreeWalkResult::Ok;
        }
        let binding = entry.to_object(&repository).unwrap();
        let blob = binding.as_blob().unwrap();
        let mut podspec: Podspec<'_> = match serde_json::from_slice(blob.content()) {
            Ok(podspec) => podspec,
            Err(e) => {
                podspecs
                    .entry(
                        entry
                            .name()
                            .unwrap()
                            .trim_end_matches(".podspec.json")
                            .to_string(),
                    )
                    .or_default()
                    .push(Res::Error {
                        error: e.to_string(),
                        path: format!("{}{}", s, entry.name().unwrap()),
                    });
                return TreeWalkResult::Ok;
            }
        };
        if podspec.prepare_command.is_none() {
            return TreeWalkResult::Ok;
        }

        podspec.loaded_from = Some(format!("{}{}", s, entry.name().unwrap()));
        podspecs
            .entry(podspec.name.to_string())
            .or_default()
            .push(Res::Podspec(podspec.into_owned()));

        TreeWalkResult::Ok
    })?;
    Ok(IterResult {
        commit: commit.id().to_string(),
        podspecs,
    })
}

fn get_dates(repo: &str) -> anyhow::Result<()> {
    let repository = Repository::open(repo)?;
    let branch = repository.find_branch("origin/master", git2::BranchType::Remote)?;
    let mut commit = branch.into_reference().peel_to_commit()?;
    let mut info: BTreeMap<String, Vec<(Delta, Oid)>> = BTreeMap::new();
    loop {
        if info.len() > 100 {
            break;
        }
        if commit.parent_count() != 1 {
            println!(
                "Commit {} has {} parents",
                commit.id(),
                commit.parent_count()
            );
            break;
        }
        let parent = commit.parent(0)?;

        let diff =
            repository.diff_tree_to_tree(Some(&parent.tree()?), Some(&commit.tree()?), None)?;
        for delta in diff.deltas() {
            let old = delta.old_file();
            let new = delta.new_file();
            let old_path = old.path().unwrap();
            let new_path = new.path().unwrap();
            if old_path != new_path {
                println!("{} -> {}", old_path.display(), new_path.display());
            }

            info.entry(new_path.display().to_string())
                .or_default()
                .push((delta.status(), commit.id()));
        }
        commit = parent;
    }
    println!("{:#?}", info);
    Ok(())
}

fn main() {
    let repo = "/Users/segiddins/Development/github.com/cocoapods/Specs";
    let specs = repo.to_owned() + "/Specs";

    let mut res = iter_repo(repo).unwrap();
    res.podspecs.values_mut().for_each(|v| {
        v.sort_by_key(|res| match res {
            Res::Podspec(podspec) => podspec.loaded_from.to_owned().unwrap(),
            Res::Error { error: _, path } => path.to_owned(),
            _ => unreachable!(),
        });
    });

    let file = File::create("podspecs_with_prepare_commands.json").unwrap();
    serde_json::to_writer_pretty(file, &res).unwrap();
    // get_dates(repo).unwrap();

    return;

    let walker = Walker::new(&specs);

    let mut specs: Vec<Res> = walker
        .par_bridge()
        .into_par_iter()
        .progress_count(800000)
        .filter(|path| path.to_string_lossy().ends_with(".podspec.json"))
        .map(|path| {
            let contents = fs::read(&path).unwrap();
            let mut podspec: Podspec = match serde_json::from_slice(&contents) {
                Ok(podspec) => podspec,
                Err(e) => {
                    return Res::Error {
                        error: e.to_string(),
                        path: path.strip_prefix(repo).unwrap().display().to_string(),
                    };
                }
            };

            if podspec.prepare_command.is_none() {
                return Res::NoPrepareCommand;
            }

            podspec.loaded_from = Some(path.strip_prefix(repo).unwrap().display().to_string());

            // podspec.published = cmd!("git", "-C", repo, "log", "-1", "--format=%cI", "--", path)
            //     .read()
            //     .unwrap()
            //     .parse()
            //     .unwrap();

            Res::Podspec(podspec.into_owned())
        })
        .filter(|res| match res {
            Res::NoPrepareCommand => false,
            _ => true,
        })
        .collect();

    specs.sort_by_key(|res| match res {
        Res::Podspec(podspec) => podspec.loaded_from.to_owned().unwrap(),
        Res::Error { error: _, path } => path.to_owned(),
        _ => unreachable!(),
    });

    let file = File::create("podspecs_with_prepare_commands.json").unwrap();
    serde_json::to_writer_pretty(file, &specs).unwrap();
}

struct Walker {
    stack: Vec<ReadDir>,
    current: Option<<Vec<PathBuf> as IntoIterator>::IntoIter>,
}

impl Walker {
    fn new(root: impl AsRef<Path>) -> Self {
        let stack = vec![fs::read_dir(root).unwrap()];
        Self {
            stack,
            current: None,
        }
    }
}

impl Iterator for Walker {
    type Item = PathBuf;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ref mut current) = self.current {
                if let Some(path) = current.next() {
                    return Some(path);
                }
                self.current = None;
            }

            if let Some(dir) = self.stack.pop() {
                let mut paths = vec![];
                for entry in dir {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    if path.is_dir() {
                        self.stack.push(fs::read_dir(&path).unwrap());
                    } else {
                        paths.push(path);
                    }
                }
                self.current = Some(paths.into_iter());
            } else {
                return None;
            }
        }
    }
}
