use std::io::Read;

use sha2::Digest;

pub mod gem {
    use std::io::BufReader;
    use std::{
        collections::HashMap,
        fmt::Display,
        io::{Read, Seek},
        marker::PhantomData,
        str::FromStr,
    };

    use anyhow::{Context, bail};
    use chrono::DateTime;
    use flate2::bufread::GzDecoder;
    use saphyr::LoadableYamlNode;
    use serde::{Deserialize, Deserializer, Serialize, de::Visitor};
    use serde_with::serde_as;
    use sha2::digest::generic_array::GenericArray;
    use strum_macros::EnumString;
    use tar::{Archive, Entry};

    fn deserialize_vec<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
    where
        T: Deserialize<'de> + FromStr,
        D: Deserializer<'de>,
        <T as FromStr>::Err: Display,
    {
        struct V<T> {
            marker: PhantomData<T>,
        }

        impl<'de, T> Visitor<'de> for V<T>
        where
            T: Deserialize<'de> + FromStr,
            <T as FromStr>::Err: Display,
        {
            type Value = Vec<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut vec = Vec::new();

                while let Some(value) = seq.next_element()? {
                    vec.push(value);
                }

                Ok(vec)
            }

            fn visit_str<E>(self, value: &str) -> Result<Vec<T>, E>
            where
                E: serde::de::Error,
            {
                let v = FromStr::from_str(value).map_err(E::custom)?;
                Ok(vec![v])
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Vec::new())
            }
        }

        let v = V {
            marker: PhantomData,
        };
        deserializer.deserialize_any(v)
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
    #[serde(deny_unknown_fields)]
    pub struct Specification {
        pub name: String,
        pub version: Version,
        pub dependencies: Vec<Dependency>,
        pub required_ruby_version: Option<Requirement>,
        pub required_rubygems_version: Option<Requirement>,
        pub rubygems_version: String,
        pub test_files: Vec<String>,
        pub specification_version: u8,
        pub summary: String,
        pub require_paths: Vec<String>,
        pub homepage: String,
        pub licenses: Vec<String>,
        #[serde(default)]
        pub metadata: HashMap<String, String>,
        pub files: Vec<String>,
        pub platform: Platform,
        pub authors: Vec<String>,
        pub autorequire: Option<String>,
        pub description: Option<String>,
        pub bindir: Option<String>,
        pub executables: Vec<String>,
        #[serde(deserialize_with = "deserialize_vec", default)]
        pub email: Vec<String>,
        pub cert_chain: Option<Vec<String>>,
        pub date: DateTime<chrono::Utc>,
        pub extensions: Vec<String>,
        pub extra_rdoc_files: Vec<String>,
        pub post_install_message: Option<String>,
        pub rdoc_options: Vec<String>,
        pub requirements: Vec<String>,
        pub signing_key: Option<String>,
        pub rubyforge_project: Option<String>,
        pub default_executable: Option<String>,
        pub has_rdoc: Option<bool>,
        pub original_platform: Option<String>,
    }

    impl Specification {
        pub fn full_name(&self) -> String {
            format!("{}-{}-{}", self.name, self.version.version, self.platform.0)
        }
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Platform(String);

    impl Platform {
        pub fn new<T: AsRef<str>>(platform: T) -> Self {
            Platform(platform.as_ref().to_string())
        }
        pub fn as_str(&self) -> &str {
            &self.0
        }
    }

    impl Default for Platform {
        fn default() -> Self {
            Platform("ruby".to_string())
        }
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum VersionSegment {
        Number(u64),
        String(String),
    }
    #[derive(Debug, PartialEq, Eq, Serialize, Default)]
    pub struct Version {
        version: String,
        #[serde(skip)]
        segments: Vec<VersionSegment>,
    }

    impl Version {
        pub fn as_str(&self) -> &str {
            &self.version
        }
    }

    impl<'de> Deserialize<'de> for Version {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            #[derive(Debug, Deserialize)]
            struct V {
                version: String,
            }
            let version = V::deserialize(deserializer)?;
            Version::from_str(&version.version).map_err(serde::de::Error::custom)
        }
    }

    impl FromStr for Version {
        type Err = anyhow::Error;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let segments: Vec<VersionSegment> = s
                .split('.')
                .map(|segment| {
                    if segment.is_empty() {
                        bail!("Empty segment in version string {:?}", s);
                    }
                    Ok(if let Ok(number) = segment.parse::<u64>() {
                        VersionSegment::Number(number)
                    } else {
                        VersionSegment::String(segment.to_string())
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Version {
                version: s.to_string(),
                segments,
            })
        }
    }
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Dependency {
        name: String,
        requirement: Requirement,
        r#type: DependencyType,
    }
    impl Dependency {
        pub fn new(name: String, requirement: Requirement, r#type: DependencyType) -> Self {
            Dependency {
                name,
                requirement,
                r#type,
            }
        }
        pub fn name(&self) -> &str {
            &self.name
        }
        pub fn requirement(&self) -> &Requirement {
            &self.requirement
        }
        pub fn r#type(&self) -> DependencyType {
            self.r#type
        }
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumString)]
    pub enum DependencyType {
        #[serde(rename = ":runtime")]
        Runtime,
        #[serde(rename = ":development")]
        Development,
    }
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Requirement {
        requirements: Vec<(RequirementOperator, Version)>,
    }

    impl Requirement {
        pub fn new(requirements: Vec<(RequirementOperator, Version)>) -> Self {
            Requirement { requirements }
        }
        pub fn requirements(&self) -> &[(RequirementOperator, Version)] {
            &self.requirements
        }
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum RequirementOperator {
        #[serde(rename = "=")]
        Equal,
        #[serde(rename = ">")]
        GreaterThan,
        #[serde(rename = ">=")]
        GreaterThanOrEqual,
        #[serde(rename = "<")]
        LessThan,
        #[serde(rename = "<=")]
        LessThanOrEqual,
        #[serde(rename = "!=")]
        NotEqual,
        #[serde(rename = "~>")]
        Tilde,
        // #[serde(untagged)]
        // Unknown(String),
        Unknown,
    }

    // enum PackageEntry {
    //     Metadata,
    //     Checksums(HashMap<String, String>),
    //     Signature(String, String),
    // }

    #[derive()]
    pub struct Package<R>
    where
        R: Read + Seek,
    {
        archive: Archive<R>,
    }

    impl<R> Package<R>
    where
        R: Read + Seek,
    {
        pub fn new(io: R) -> Package<R> {
            let archive = tar::Archive::new(io);
            Package { archive }
        }

        pub fn specification(&mut self) -> anyhow::Result<Specification> {
            let mut entries = self.archive.entries_with_seek()?;
            let entry = entries
                .find(|entry| {
                    let entry = entry.as_ref().unwrap();
                    entry.path().unwrap().to_str() == Some("metadata.gz")
                })
                .expect("metadata.gz")?;

            let mut reader = flate2::read::GzDecoder::new(entry);

            let mut contents: String = String::new();
            reader.read_to_string(&mut contents)?;

            let specification: Specification =
                serde_yaml::from_str(&contents).inspect_err(|err| {
                    saphyr::Yaml::load_from_str(&contents)
                        .inspect_err(|err| {
                            eprintln!("Failed to parse YAML: {:#?}", err);
                        })
                        .inspect(|yaml| {
                            println!("YAML: {yaml:#?}");
                        })
                        .unwrap();
                    // for (idx, line) in contents.lines().enumerate() {
                    //     let idx = idx + 1;
                    //     println!("{idx:>3}| {line}");
                    // }
                })?;

            self.archive.reset()?;
            Ok(specification)
        }

        pub fn each_entry(
            &mut self,
            mut f: impl FnMut(&mut Entry<GzDecoder<BufReader<Entry<R>>>>) -> anyhow::Result<()>,
        ) -> anyhow::Result<()> {
            let mut entries = self.archive.entries_with_seek()?;

            let entry = entries
                .find(|entry| {
                    let entry = entry.as_ref().unwrap();
                    entry.path().unwrap().to_str() == Some("data.tar.gz")
                })
                .context("data.tar.gz")??;

            let reader = flate2::bufread::GzDecoder::new(BufReader::new(entry));
            let mut archive = tar::Archive::new(reader);
            let entries = archive.entries()?;
            for entry in entries {
                let mut entry = entry?;
                f(&mut entry)?;
            }

            Ok(())
        }
    }

    #[serde_as]
    #[derive(Debug, PartialEq, Eq, Serialize)]
    pub struct PackageEntry<'a> {
        pub gem: &'a str,
        pub version: &'a str,
        pub platform: &'a str,
        pub size: u64,
        pub path: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub link_name: Option<&'a str>,
        pub mode: u32,
        #[serde(skip_serializing)]
        pub uid: u64,
        #[serde(skip_serializing)]
        pub gid: u64,
        pub mtime: u64,
        #[serde_as(as = "serde_with::hex::Hex")]
        pub sha256: GenericArray<u8, <sha2::Sha256 as sha2::digest::OutputSizeUser>::OutputSize>,
        pub magic: &'a str,
    }
}
