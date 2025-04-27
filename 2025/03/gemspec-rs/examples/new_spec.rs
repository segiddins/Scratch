#![feature(assert_matches)]

use std::{
    assert_matches::assert_matches, borrow::Cow, default, fs::File, io::Read, marker::PhantomData,
    os::macos::raw::stat, str::FromStr,
};

use anyhow::{Context, bail};
use gemspec_rs::gem::{Dependency, DependencyType, Platform, Requirement, Specification, Version};
use saphyr::{Scalar, Tag};
use saphyr_parser::{Event, EventReceiver, Span, SpannedEventReceiver};
use strum_macros::EnumString;

fn main() {
    let cache = std::path::Path::new("/Users/segiddins/.gem/ruby/3.3.5/cache");
    // let path = std::path::Path::new("/Users/segiddins/.gem/jruby/3.1.4/cache/bundler-2.5.22.gem");

    let binding = cache
        .read_dir()
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path().to_owned();
            if path.extension().is_some_and(|ext| ext == "gem") {
                Some(path)
            } else {
                None
            }
        })
        .take(1)
        .collect::<Vec<_>>();
    let path = binding.first().unwrap();
    let file = File::open(&path).unwrap();

    let mut archive = tar::Archive::new(file);
    let mut entries = archive.entries_with_seek().unwrap();

    let entry = entries
        .find(|entry| {
            let entry = entry.as_ref().unwrap();
            entry.path().unwrap().to_str() == Some("metadata.gz")
        })
        .expect("metadata.gz")
        .unwrap();

    let mut reader = flate2::read::GzDecoder::new(entry);
    let mut contents: String = String::new();
    reader.read_to_string(&mut contents).unwrap();

    let mut parser = saphyr_parser::Parser::new_from_str(&contents);
    for (idx, line) in contents.lines().enumerate() {
        let idx = idx + 1;
        println!("{idx:>3}| {line}");
    }

    assert_matches!(parser.next(), Some(Ok((Event::StreamStart, _))));
    assert_matches!(parser.next(), Some(Ok((Event::DocumentStart(_), _))));
    assert_matches!(
        parser.next(),
        Some(Ok((Event::MappingStart(0, Some(tag)), _,))) if ruby_object_tag(&tag, "Gem::Specification")
    );
    parse_gem_specification(&mut parser).unwrap();

    assert_matches!(parser.next(), Some(Ok((Event::DocumentEnd, _))));
    assert_matches!(parser.next(), Some(Ok((Event::StreamEnd, _))));
    assert_matches!(parser.next(), None);
    // let mut level = 0;

    // for event in parser {
    //     match event {
    //         Ok((event, _)) => {
    //             println!("{}{event:?}", "  ".repeat(level));
    //             match event {
    //                 saphyr_parser::Event::StreamStart
    //                 | saphyr_parser::Event::DocumentStart(_)
    //                 | saphyr_parser::Event::MappingStart(_, _)
    //                 | saphyr_parser::Event::SequenceStart(_, _) => {
    //                     level += 1;
    //                 }
    //                 saphyr_parser::Event::StreamEnd
    //                 | saphyr_parser::Event::DocumentEnd
    //                 | saphyr_parser::Event::MappingEnd
    //                 | saphyr_parser::Event::SequenceEnd => {
    //                     level -= 1;
    //                 }

    //                 _ => {}
    //             }
    //         }
    //         Err(err) => {
    //             eprintln!("Error: {err}");
    //         }
    //     }
    // }

    // let spec = builder.build();
    // println!("spec: {spec:?}");
}

// trait Receiver<'input, Output>: SpannedEventReceiver<'input> {
//     fn build(self) -> anyhow::Result<Output>;
// }

// #[derive(Default)]
// struct SingleDocumentReceiver<'input, Output, R>
// where
//     R: Receiver<'input, Output>,
// {
//     receiver: R,
//     state: u8,
//     marker: PhantomData<&'input Output>,
// }

// impl<'input, O, R> SpannedEventReceiver<'input> for SingleDocumentReceiver<'input, O, R>
// where
//     R: Receiver<'input, O>,
// {
//     fn on_event(&mut self, ev: saphyr_parser::Event<'input>, span: saphyr_parser::Span) {
//         match (self.state, &ev) {
//             (0, saphyr_parser::Event::StreamStart) => {
//                 self.state = 1;
//             }
//             (1, saphyr_parser::Event::DocumentStart(_)) => {
//                 self.state = 2;
//             }
//             (2, saphyr_parser::Event::DocumentEnd) => {
//                 self.state = 3;
//             }
//             (3, saphyr_parser::Event::StreamEnd) => {
//                 self.state = 4;
//             }
//             (2, _) => {
//                 self.receiver.on_event(ev, span);
//             }
//             _ => unreachable!(),
//         }
//     }
// }

// impl<'a, Output, R> Receiver<'a, Output> for SingleDocumentReceiver<'a, Output, R>
// where
//     R: Receiver<'a, Output>,
// {
//     fn build(self) -> anyhow::Result<Output> {
//         assert!(self.state == 4);
//         self.receiver.build()
//     }
// }

#[derive(Default)]
enum MappingState<'input> {
    #[default]
    Value,
    Key(Cow<'input, str>),
}

impl<'input> MappingState<'input> {
    fn take(&mut self) -> Option<Cow<'input, str>> {
        match self {
            MappingState::Key(key) => {
                let ret = std::mem::take(key);
                *self = MappingState::Value;
                Some(ret)
            }
            MappingState::Value => None,
        }
    }
}

#[derive(Default)]
struct VersionReceiver<'input> {
    mapping: MappingState<'input>,
    version: Cow<'input, str>,
}

impl<'input> EventReceiver<'input> for VersionReceiver<'input> {
    fn on_event(&mut self, event: saphyr_parser::Event<'input>) {
        match (self.mapping.take(), event) {
            (None, saphyr_parser::Event::Scalar(value, style, aid, tag)) => {
                self.mapping = MappingState::Key(value);
            }
            (Some(key), saphyr_parser::Event::Scalar(value, style, aid, tag))
                if key == "version" =>
            {
                self.version = value;
            }
            (key, event) => unimplemented!(
                "Event {:?} not implemented in VersionReceiver {:?}",
                event,
                key
            ),
        }
    }
}

#[derive(Default)]
struct SpecificationReceiver<'input> {
    mapping: MappingState<'input>,
    name: Option<Cow<'input, str>>,
    version: Option<VersionReceiver<'input>>,
}

impl<'input> EventReceiver<'input> for SpecificationReceiver<'input> {
    fn on_event(&mut self, event: saphyr_parser::Event<'input>) {
        match (self.mapping.take(), event) {
            (None, saphyr_parser::Event::Scalar(value, style, aid, tag)) => {
                self.mapping = MappingState::Key(value);
            }
            (Some(key), saphyr_parser::Event::Scalar(value, style, aid, tag)) if key == "name" => {
                self.name = Some(value);
            }
            (key, event) => unimplemented!(
                "Event {:?} not implemented in SpecificationReceiver {:?}",
                event,
                key
            ),
        }
    }
}

#[derive(Default)]
struct Receiver<'input> {
    stack: Vec<Box<dyn EventReceiver<'input> + 'input>>,
}

impl<'input> EventReceiver<'input> for Receiver<'input> {
    fn on_event(&mut self, event: saphyr_parser::Event<'input>) {
        match event {
            saphyr_parser::Event::StreamStart => {}
            saphyr_parser::Event::DocumentStart(_) => {}
            saphyr_parser::Event::MappingStart(_, Some(tag))
                if ruby_object_tag(&tag, "Gem::Specification") =>
            {
                self.stack.push(Box::new(SpecificationReceiver::default()));
            }
            // saphyr_parser::Event::MappingStart(_, Some(tag))
            //     if ruby_object_tag(&tag, "Gem::Version") =>
            // {
            //     self.stack.push(Box::new(VersionReceiver::default()));
            // }
            event if !self.stack.is_empty() => {
                let receiver = self.stack.last_mut().unwrap();
                receiver.on_event(event);
                // receiver.on_event(event);
                // if matches!(event, saphyr_parser::Event::MappingEnd) {
                //     self.stack.pop();
                // }
            }
            _ => unimplemented!(
                "Event {:?} not implemented in Receiver {}",
                event,
                self.stack.len()
            ),
        }
    }
}

fn ruby_object_tag(tag: &Tag, name: &str) -> bool {
    tag.handle == "!"
        && tag
            .suffix
            .strip_prefix("ruby/object:")
            .is_some_and(|s| s == name)
}

fn parse_str<'input>(event: saphyr_parser::Event<'input>) -> anyhow::Result<Cow<'input, str>> {
    match event {
        saphyr_parser::Event::Scalar(value, style, 0, None) => {
            match saphyr::Scalar::parse_from_cow_and_metadata(value, style, None) {
                Some(Scalar::String(str)) => Ok(str),
                scalar => bail!("Expected a string, got {:?}", scalar),
            }
        }
        _ => bail!("Expected a scalar, got {:?}", event),
    }
}

fn parse_integer<'input>(event: saphyr_parser::Event<'input>) -> anyhow::Result<i64> {
    match event {
        saphyr_parser::Event::Scalar(value, style, 0, None) => {
            match saphyr::Scalar::parse_from_cow_and_metadata(value, style, None) {
                Some(Scalar::Integer(int)) => Ok(int),
                scalar => bail!("Expected a string, got {:?}", scalar),
            }
        }
        _ => bail!("Expected a scalar, got {:?}", event),
    }
}

fn parse_null<'input>(event: saphyr_parser::Event<'input>) -> anyhow::Result<()> {
    match event {
        saphyr_parser::Event::Scalar(value, _, 0, None) => {
            match saphyr::Scalar::parse_from_cow(value) {
                Scalar::Null => Ok(()),
                scalar => bail!("Expected null, got {:?}", scalar),
            }
        }
        _ => Err(anyhow::anyhow!("Expected a scalar")),
    }
}

fn parse_gem_version<'input, I>(parser: &mut I) -> anyhow::Result<Version>
where
    I: Iterator<Item = Result<(saphyr_parser::Event<'input>, Span), saphyr_parser::ScanError>>,
{
    #[derive(Debug)]
    enum State {
        Key,
        Version,
    }

    let mut state = State::Key;
    let mut version: Option<Cow<'input, str>> = None;

    loop {
        let Some(event) = parser.next() else {
            bail!("Expected more events");
        };
        match (state, event?.0) {
            (State::Key, Event::MappingEnd) => {
                // End of the mapping
                return version
                    .ok_or_else(|| anyhow::anyhow!("Expected version"))?
                    .parse();
            }

            (State::Key, event) => match parse_str(event)?.as_ref() {
                "version" => {
                    state = State::Version;
                }
                key => {
                    bail!("Expected version, got {:?}", key);
                }
            },
            (State::Version, event) => {
                version = Some(parse_str(event).context("version version number")?);
                state = State::Key;
            }

            (state, event) => unimplemented!(
                "Event {:?} not implemented in parse_gem_version {:?}",
                event,
                state
            ),
        }
    }
}

fn parse_gem_requirement<'input, I>(parser: &mut I) -> anyhow::Result<Requirement>
where
    I: Iterator<Item = Result<(saphyr_parser::Event<'input>, Span), saphyr_parser::ScanError>>,
{
    #[derive(Debug, EnumString)]
    enum Key {
        #[strum(to_string = "requirements")]
        Requirements,
    }

    let mut state = None;
    let mut requirements = vec![];

    loop {
        let Some(event) = parser.next() else {
            bail!("Expected more events");
        };
        match (state, event?.0) {
            (None, Event::MappingEnd) => {
                // End of the mapping
                return Ok(Requirement::new(requirements));
            }

            (None, event) => {
                let key = parse_str(event)?;
                state = Some(
                    Key::from_str(key.as_ref())
                        .with_context(|| format!("unknown Gem::Requirement ivar {key:?}"))?,
                );
            }

            (Some(Key::Requirements), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        Event::SequenceStart(_, None) => {
                            let op = parser.next().expect("requirement op")?.0;
                            let op = parse_str(op).context("requirement op")?;
                            let version = parser.next().expect("requirement version")?.0;

                            assert_matches!(version, Event::MappingStart(_, Some(tag)) if ruby_object_tag(&tag, "Gem::Version"));
                            let version =
                                parse_gem_version(parser).context("requirement version")?;

                            let seq_end = parser.next().expect("requirement seq end")?.0;
                            assert_matches!(seq_end, Event::SequenceEnd);
                        }

                        event => {
                            let str = parse_str(event).context("parsing requirement")?;
                        }
                    }
                }
                state = None;
            }

            (state, event) => unimplemented!(
                "Event {:?} not implemented in parse_gem_requirement {:?}",
                event,
                state
            ),
        }
    }
}

fn parse_dependency<'input, I>(parser: &mut I) -> anyhow::Result<Dependency>
where
    I: Iterator<Item = Result<(saphyr_parser::Event<'input>, Span), saphyr_parser::ScanError>>,
{
    #[derive(Debug, EnumString)]
    enum Key {
        #[strum(to_string = "name")]
        Name,
        #[strum(to_string = "requirement")]
        Requirement,
        #[strum(to_string = "type")]
        Type,

        #[strum(to_string = "prerelease")]
        Prerelease,
        #[strum(to_string = "version_requirements")]
        VersionRequirements,
    }

    let mut state = None;

    let mut name: Option<Cow<'input, str>> = None;
    let mut requirement: Option<Requirement> = None;
    let mut dep_type: Option<DependencyType> = None;

    loop {
        let Some(event) = parser.next() else {
            bail!("Expected more events");
        };
        match (state, event?.0) {
            (None, Event::MappingEnd) => {
                // End of the mapping
                return Ok(Dependency::new(
                    name.unwrap().to_string(),
                    requirement.unwrap(),
                    dep_type.unwrap(),
                ));
            }

            (None, event) => {
                let key = parse_str(event)?;
                state = Some(
                    Key::from_str(key.as_ref())
                        .with_context(|| format!("unknown Gem::Dependency ivar {key:?}"))?,
                );
            }

            (Some(Key::Name), event) => {
                name = Some(parse_str(event).context("parsing dependency name")?);
                state = None;
            }

            (Some(Key::Requirement), event) => {
                requirement = Some(parse_gem_requirement(parser)?);
                state = None;
            }

            (Some(Key::Type), event) => {
                let type_str = parse_str(event).context("parsing dependency type")?;
                match type_str.as_ref() {
                    ":runtime" => dep_type = Some(DependencyType::Runtime),
                    ":development" => dep_type = Some(DependencyType::Development),

                    _ => bail!("Unknown dependency type {type_str}"),
                }
                // dep_type = Some(DependencyType::from_str(type_str.as_ref()).unwrap());
                state = None;
            }
            (Some(Key::Prerelease), Event::Scalar(_, _, 0, None)) => {
                state = None;
            }
            (Some(Key::VersionRequirements), Event::MappingStart(0, Some(tag)))
                if ruby_object_tag(&tag, "Gem::Requirement") =>
            {
                let requirement = parse_gem_requirement(parser)?;
                state = None;
            }

            (state, event) => unimplemented!(
                "Event {:?} not implemented in parse_dependency {:?}",
                event,
                state
            ),
        }
    }
}

// fn parse_list_of<'input, I, F, R>(parser: &mut I, f: F) -> anyhow::Result<Vec<R>>
// where
//     I: Iterator<Item = Result<(saphyr_parser::Event<'input>, Span), saphyr_parser::ScanError>>,
//     F: Fn(&mut I) -> anyhow::Result<R>,
// {
//     let mut list = Vec::new();
//     loop {
//         let Some(event) = parser.next() else {
//             bail!("Expected more events");
//         };
//         match event?.0 {
//             Event::SequenceEnd => break,
//             event => {
//                 let item = f(chain([event], parser))?;
//                 list.push(item);
//             }
//         }
//     }
//     Ok(list)
// }

fn parse_gem_specification<'input, I>(parser: &mut I) -> anyhow::Result<Specification>
where
    I: Iterator<Item = Result<(saphyr_parser::Event<'input>, Span), saphyr_parser::ScanError>>,
{
    #[derive(Debug, EnumString)]
    #[strum(serialize_all = "snake_case")]
    enum State {
        Name,
        Version,
        Platform,
        Authors,
        Autorequire,
        Bindir,
        CertChain,
        Date,
        Dependencies,
        Description,
        Email,
        Executables,
        Extensions,
        ExtraRdocFiles,
        Files,
        Homepage,
        Licenses,
        Metadata,
        PostInstallMessage,
        RdocOptions,
        RequirePaths,
        RequiredRubyVersion,
        RequiredRubygemsVersion,
        Requirements,
        RubygemsVersion,
        SigningKey,
        SpecificationVersion,
        Summary,
        TestFiles,
    }

    let mut state = None;

    let mut name: Option<Cow<'input, str>> = None;
    let mut version: Option<Version> = None;
    let mut platform: Option<Platform> = None;
    let mut authors: Vec<String> = Vec::new();
    let mut bindir: Option<Cow<'input, str>> = None;
    let mut cert_chain: Option<Vec<String>> = None;
    let mut description: Option<Cow<'input, str>> = None;

    loop {
        let Some(event) = parser.next() else {
            bail!("Expected more events");
        };
        match (state, event?.0) {
            (None, Event::MappingEnd) => {
                // End of the mapping
                return Ok(Specification {
                    name: name.unwrap().to_string(),
                    version: version.unwrap(),
                    platform: platform.unwrap(),
                    authors,
                    bindir: bindir.map(|s| s.to_string()),
                    cert_chain,
                    description: description.map(|s| s.to_string()),
                    ..Default::default()
                });
            }

            (None, event) => match parse_str(event)?.as_ref() {
                str => {
                    state = Some(
                        State::from_str(str)
                            .with_context(|| format!("unknown Gem::Specification ivar {str:?}"))?,
                    )
                }
            },
            (Some(State::Name), event) => {
                name = Some(parse_str(event)?);
                state = None;
            }
            (Some(State::Version), Event::MappingStart(0, Some(tag)))
                if ruby_object_tag(&tag, "Gem::Version") =>
            {
                version = Some(parse_gem_version(parser)?);
                state = None;
            }

            (Some(State::Platform), event) => {
                platform = Some(Platform::new(parse_str(event)?));
                state = None;
            }

            (Some(State::Authors), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let author = parse_str(event)?;
                            authors.push(author.to_string());
                        }
                    }
                }
                state = None;
            }

            (Some(State::Autorequire), event) => {
                parse_null(event)?;
                // autorequire = parse_str(event)?;
                state = None;
            }

            (Some(State::Bindir), event) => {
                bindir = Some(parse_str(event)?);
                state = None;
            }

            (Some(State::CertChain), Event::SequenceStart(_, None)) => {
                cert_chain = Some(vec![]);
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let cert = parse_str(event)?;
                            cert_chain.as_mut().unwrap().push(cert.to_string());
                        }
                    }
                }
                state = None;
            }

            (Some(State::Date), event) => {
                let date = parse_str(event)?;
                state = None;
            }

            (Some(State::Dependencies), Event::SequenceStart(_, None)) => {
                let mut dependencies = vec![];
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        Event::MappingStart(_, Some(tag))
                            if ruby_object_tag(&tag, "Gem::Dependency") =>
                        {
                            let dependency = parse_dependency(parser)?;
                            dependencies.push(dependency);
                        }

                        event => {
                            bail!("Expected a dependency, got {:?}", event);
                        }
                    }
                }
                // dependencies = parse_list_of(event, parse_gem_version)?;
                state = None;
            }

            (Some(State::Description), event) => {
                description = Some(parse_str(event)?);
                state = None;
            }

            (Some(State::Email), event) => {
                let email = parse_str(event)?;
                state = None;
            }

            (Some(State::Executables), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let executable = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::Extensions), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let extension = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::ExtraRdocFiles), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let extension = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::Files), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let extension = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::Homepage), event) => {
                let homepage = parse_str(event)?;
                state = None;
            }

            (Some(State::Licenses), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let license = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::Metadata), Event::MappingStart(0, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::MappingEnd => break,

                        event => {
                            let key = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::PostInstallMessage), event) => {
                parse_null(event)?;
                state = None;
            }

            (Some(State::RdocOptions), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let option = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::RequirePaths), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let path = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::RequiredRubyVersion), Event::MappingStart(0, Some(tag)))
                if ruby_object_tag(&tag, "Gem::Requirement") =>
            {
                parse_gem_requirement(parser)?;
                state = None;
            }
            (Some(State::RequiredRubygemsVersion), Event::MappingStart(0, Some(tag)))
                if ruby_object_tag(&tag, "Gem::Requirement") =>
            {
                parse_gem_requirement(parser)?;
                state = None;
            }

            (Some(State::Requirements), Event::SequenceStart(0, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let requirement = parse_gem_requirement(parser)?;
                        }
                    }
                }
                state = None;
            }

            (Some(State::RubygemsVersion), event) => {
                let rubygems_version = parse_str(event)?;
                state = None;
            }
            (Some(State::SigningKey), event) => {
                let signing_key = parse_null(event)?;
                state = None;
            }

            (Some(State::SpecificationVersion), event) => {
                let specification_version = parse_integer(event)?;
                state = None;
            }

            (Some(State::Summary), event) => {
                let summary = parse_str(event)?;
                state = None;
            }
            (Some(State::TestFiles), Event::SequenceStart(_, None)) => {
                while let Some(event) = parser.next() {
                    match event?.0 {
                        Event::SequenceEnd => break,

                        event => {
                            let test_file = parse_str(event)?;
                        }
                    }
                }
                state = None;
            }

            (state, event) => unimplemented!(
                "Event {:?} not implemented in parse_gem_specification {:?}",
                event,
                state
            ),
        }
    }
}
