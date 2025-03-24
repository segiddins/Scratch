use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    ops::Range,
    path::{self, Path, PathBuf},
};

use anyhow::bail;
use assoc::AssocExt;
use chrono::{DateTime, Utc};
use git2::{Commit, Delta, ObjectType, Oid, Repository, Tree, TreeEntry, TreeIter, TreeWalkResult};
use rayon::{iter::IterBridge, prelude::*};
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
    println!("Fetching...");
    remote.fetch(&["master"], None, None)?;
    let branch = repository.find_branch("origin/master", git2::BranchType::Remote)?;
    let commit = branch.get().peel_to_commit()?;
    println!("Commit: {}", commit.id());

    {
        println!("Finding dates...");
        let mut c = commit.clone();
        loop {
            // let out = format!("{}: {:?} ({:?})", c.id(), c.time(), c.summary());
            let (p, d) = thing(&repository, &c)?;
            // println!("{}: {:?}", out, d);
            c = p;
            break;
        }
    }

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

    let mut lhs_entries: Vec<(String, TreeEntry)> =
        lhs.as_ref().map_or_else(Default::default, |t| {
            t.iter()
                .map(|e| (e.name().unwrap().to_string(), e))
                .collect()
        });

    let mut rhs_entries: Vec<(String, TreeEntry)> =
        rhs.as_ref().map_or_else(Default::default, |t| {
            t.iter()
                .map(|e| (e.name().unwrap().to_string(), e))
                .collect()
        });

    lhs_entries.sort_by(|(l, _), (r, _)| l.cmp(r));
    rhs_entries.sort_by(|(l, _), (r, _)| l.cmp(r));

    let all_keys = lhs_entries
        .iter()
        .map(|(k, _)| k)
        .chain(rhs_entries.iter().map(|(k, _)| k))
        .collect::<HashSet<_>>();

    let mut res: Vec<(Delta, String)> = vec![];

    for name in all_keys {
        let l = lhs_entries.get(name);
        let r = rhs_entries.get(name);

        if l.as_ref().map(|e| e.id()) == r.as_ref().map(|e| e.id()) {
            continue;
        }

        let l_kind = l.as_ref().and_then(|e| e.kind());
        let r_kind = r.as_ref().and_then(|e| e.kind());

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

    Ok(res)
}

fn thing<'a>(
    repository: &'a Repository,
    commit: &Commit<'a>,
) -> anyhow::Result<(Commit<'a>, Vec<(Delta, String)>)> {
    if commit.parent_count() != 1 {
        bail!(
            "Commit {} has {} parents\n{}",
            commit.id(),
            commit.parent_count(),
            commit.body().unwrap_or_default()
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

#[derive(Debug, Deserialize)]
struct CocoaPodsVersion {
    min: String,
    max: String,
    prefix_lengths: Vec<usize>,
}

// fn specs_par_iter<'a>(repo: &'a Repository, tree: Tree<'a>) -> () {
//     let cocoapods_version: TreeEntry<'_> = tree.get_name("CocoaPods-version.yml").unwrap();
//     let cocoapods_version = cocoapods_version
//         .to_object(repo)
//         .unwrap()
//         .into_blob()
//         .unwrap();
//     let cocoapods_version: CocoaPodsVersion =
//         serde_yaml::from_slice(cocoapods_version.content()).unwrap();

//     let specs = tree.get_name("Specs").unwrap();
//     let specs = specs.to_object(repo).unwrap().into_tree().unwrap();

//     let trees: Vec<Tree<'a>> = vec![];

//     specs.iter().par_bridge()

//     todo!()
// }

fn get_dates(repo: &str) -> anyhow::Result<BTreeMap<String, Vec<(Delta, Oid)>>> {
    let repository = Repository::open(repo)?;
    let branch = repository.find_branch("origin/master", git2::BranchType::Remote)?;
    let mut commit = branch.into_reference().peel_to_commit()?;
    let mut info: BTreeMap<String, Vec<(Delta, Oid)>> = BTreeMap::new();
    loop {
        match commit.parent_count() {
            0 => break,
            1 => {}
            n => bail!("Commit {} has {} parents", commit.id(), n),
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
    Ok(info)
}

struct PodspecIterMap<'repo, T> {
    repository: &'repo Repository,
    iter: Option<(Tree<'repo>, PathBuf, Range<usize>)>,
    stack: Vec<(Tree<'repo>, PathBuf)>,
    func: Box<dyn Fn(&'repo Repository, &Path, TreeEntry) -> T + 'static>,
}

impl<'repo, T> PodspecIterMap<'repo, T> {
    fn new<F>(repository: &'repo Repository, func: F) -> Result<Self, git2::Error>
    where
        F: Fn(&'repo Repository, &Path, TreeEntry) -> T + 'static,
    {
        let branch = repository.find_branch("origin/master", git2::BranchType::Remote)?;
        let commit = branch.get().peel_to_commit()?;
        let tree = commit.tree()?;
        let range = 0..tree.len();

        Ok(Self {
            repository,
            iter: Some((tree, "".into(), range)),
            stack: vec![],
            func: Box::new(func),
        })
    }
}

impl<T> Iterator for PodspecIterMap<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((tree, dir, range)) = self.iter.as_mut() {
                if let Some(entry) = (*range).next().map(|i| tree.get(i).unwrap()) {
                    let dir = dir.join(entry.name().unwrap());
                    match entry.kind() {
                        Some(ObjectType::Tree) => {
                            let tree = entry
                                .to_object(self.repository)
                                .unwrap()
                                .into_tree()
                                .unwrap();
                            self.stack.push((tree, dir));
                            continue;
                        }
                        Some(ObjectType::Blob)
                            if entry.name().unwrap().ends_with(".podspec.json") =>
                        {
                            return Some((self.func)(self.repository, &dir, entry));
                        }
                        _ => {
                            continue;
                        }
                    }
                }
            }
            let (tree, dir) = self.stack.pop()?;
            let range = 0..tree.len();
            self.iter = Some((tree, dir, range));
        }
    }
}

fn main() {
    let repo = "/Users/segiddins/Development/github.com/cocoapods/Specs";

    let repository = Repository::open(repo).unwrap();
    let iter = PodspecIterMap::new(&repository, |repo, path, entry| {
        let binding = entry.to_object(repo).unwrap();
        let blob = binding.into_blob().unwrap();
        let podspec: Res = serde_json::from_slice(blob.content())
            .map(|podspec: Podspec| {
                if podspec.prepare_command.is_some() {
                    Res::Podspec(podspec.into_owned())
                } else {
                    Res::NoPrepareCommand
                }
            })
            .unwrap_or_else(|e| Res::Error {
                error: e.to_string(),
                path: path.display().to_string(),
            });
        podspec
    })
    .unwrap();
    // println!("{:#?}", iter.collect::<Vec<_>>().len());

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
}
