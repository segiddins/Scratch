use std::{
    borrow::Cow,
    cmp::max,
    collections::{BTreeMap, HashSet},
    fmt::{Display, format},
    fs::{self, File, ReadDir},
    iter::zip,
    ops::Deref,
    path::{Path, PathBuf},
};

use anyhow::bail;
use assoc::AssocExt;
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

    // {
    //     let mut c = commit.clone();
    //     let mut i = 10000000;
    //     while i > 0 {
    //         c.time().
    //         let out = format!("{}: {:?} ({:?})", c.id(), c.time(), c.summary());
    //         let (p, d) = thing(&repository, c)?;
    //         println!("{}: {:?}", out, d);
    //         c = p;
    //         i -= 1;
    //     }
    // }

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

fn tree_diff<'a>(
    repository: &'a Repository,
    path: &str,
    lhs: Option<Tree<'a>>,
    rhs: Option<Tree<'a>>,
) -> anyhow::Result<Vec<(Delta, String)>> {
    if lhs.as_ref().map(|t| t.id()) == rhs.as_ref().map(|t| t.id()) {
        return Ok(vec![]);
    }

    let mut lhs_entries: Vec<(String, TreeEntry)> = lhs.as_ref().map_or_else(
        || Default::default(),
        |t| {
            t.iter()
                .map(|e| (e.name().unwrap().to_string(), e))
                .collect()
        },
    );

    let mut rhs_entries: Vec<(String, TreeEntry)> = rhs.as_ref().map_or_else(
        || Default::default(),
        |t| {
            t.iter()
                .map(|e| (e.name().unwrap().to_string(), e))
                .collect()
        },
    );

    lhs_entries.sort_by(|(l, _), (r, _)| l.cmp(r));
    rhs_entries.sort_by(|(l, _), (r, _)| l.cmp(r));

    let mut all_keys = lhs_entries
        .iter()
        .map(|(k, _)| k)
        .chain(rhs_entries.iter().map(|(k, _)| k))
        .collect::<HashSet<_>>();

    let mut res: Vec<(Delta, String)> = vec![];

    for name in all_keys {
        let l = lhs_entries.get(name);
        let r = rhs_entries.get(name);

        if l.as_ref().map(|(e)| e.id()) == r.as_ref().map(|(e)| e.id()) {
            continue;
        }

        let l_kind = l.as_ref().map(|(e)| e.kind()).flatten();
        let r_kind = r.as_ref().map(|(e)| e.kind()).flatten();

        let child_path = format!("{}/{}", path, name);

        match (l_kind, r_kind) {
            (Some(ObjectType::Blob), Some(ObjectType::Blob)) => {
                res.push((Delta::Modified, child_path));
            }
            (None, Some(ObjectType::Blob)) => {
                res.push((Delta::Added, child_path));
            }
            (Some(ObjectType::Blob), None) => {
                res.push((Delta::Deleted, child_path));
            }
            (None, Some(ObjectType::Tree)) | (Some(ObjectType::Tree), _) => {
                let mut diff = tree_diff(
                    repository,
                    child_path.as_str(),
                    l.map(|l| l.to_object(repository).unwrap().into_tree().unwrap()),
                    r.map(|l| l.to_object(repository).unwrap().into_tree().unwrap()),
                )?;
                res.append(&mut diff);
            }
            (None, None) => unreachable!(),
            (l, r) => {
                bail!("unimplemented for {}: {:?}, {:?}", child_path, l, r);
            }
        }
    }

    return Ok(res);
}

fn thing<'a>(
    repository: &'a Repository,
    commit: Commit<'a>,
) -> anyhow::Result<(Commit<'a>, Vec<(Delta, String)>)> {
    if commit.parent_count() != 1 {
        bail!(
            "Commit {} has {} parents",
            commit.id(),
            commit.parent_count()
        );
    }
    let parent = commit.parent(0)?;
    let tree = commit.tree()?;

    let parent_tree = parent.tree()?;

    Ok((
        parent,
        tree_diff(repository, ".", Some(parent_tree), Some(tree))?,
    ))
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
