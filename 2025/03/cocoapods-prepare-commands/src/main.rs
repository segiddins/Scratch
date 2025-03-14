use std::{
    borrow::Cow,
    fs::{self, File, ReadDir},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use duct::cmd;
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

fn main() {
    let repo = "/Users/segiddins/Development/github.com/cocoapods/Specs";
    let specs = repo.to_owned() + "/Specs";

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
