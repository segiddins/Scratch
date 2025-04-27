use std::{fs::File, io::Read, os::unix::fs::MetadataExt, sync::atomic::AtomicU32};

use anyhow::Result;
use gemspec_rs::gem::{Package, PackageEntry};
use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
use sha2::Digest;
use std::sync::atomic::Ordering::SeqCst;

struct GemSummary {
    name: String,
    version: String,
    platform: String,
    size: u64,

    metadata_sha256: String,
    files: u64,
    sha256: String,
    source_date_epoch: u64,
    rubygems_version: String,
}

fn main() -> Result<()> {
    let cache = std::path::Path::new("/Users/segiddins/.gem/ruby/3.3.5/cache");
    // let path = std::path::Path::new("/Users/segiddins/.gem/jruby/3.1.4/cache/bundler-2.5.22.gem");

    let count: AtomicU32 = 0.into();

    cache
        .read_dir()?
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "gem") {
                Some(path)
            } else {
                None
            }
        })
        .par_bridge()
        .into_par_iter()
        .for_each(|path| {
            let file = File::open(&path).unwrap();
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut package = Package::new(file);

            let spec = match package.specification() {
                Ok(spec) => spec,
                Err(err) => {
                    eprintln!(
                        "Failed to read specification {:?}: {:#?}",
                        path.display(),
                        err
                    );
                    return;
                }
            };

            // let mut summary = GemSummary {
            //     name: spec.name.clone(),
            //     version: spec.version.to_string(),
            //     platform: spec.platform.to_string(),
            //     size: file.metadata()?.size(),
            //     metadata_sha256: String::new(),
            //     files: 0,
            //     sha256: String::new(),
            //     source_date_epoch: 0,
            //     rubygems_version: String::new(),
            // };

            package
                .each_entry(|e| {
                    let mut buf = Vec::new();
                    e.read_to_end(&mut buf)?;

                    let sha256 = sha2::Sha256::digest(&buf);
                    let magic = tree_magic_mini::from_u8(&buf);

                    let header = e.header();
                    let path = header.path().unwrap();
                    let link_name = header.link_name()?;

                    let entry = PackageEntry {
                        gem: spec.name.as_str(),
                        version: spec.version.as_str(),
                        platform: spec.platform.as_str(),
                        size: header.size()?,
                        path: path.to_str().unwrap(),
                        link_name: link_name.as_ref().map(|s| s.to_str().unwrap()),
                        mode: header.mode()?,
                        uid: header.uid()?,
                        gid: header.gid()?,
                        mtime: header.mtime()?,
                        sha256,
                        magic,
                    };
                    // println!("{}", serde_json::to_string(&entry).unwrap());
                    Ok(())
                })
                .unwrap();
        });

    println!("Processed {} gem files", count.load(SeqCst));

    Ok(())
}
