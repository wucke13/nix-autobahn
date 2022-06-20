use std::{
    collections::HashMap,
    fs,
    io::{self, prelude::*},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use clap::Parser;
use indicatif::{
    ParallelProgressIterator, ProgressBar, ProgressFinish, ProgressIterator, ProgressStyle,
};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use regex::bytes::Regex;

const NIX_BUILD_FHS: &str = "nix-build --no-out-link -E";
const LDD_NOT_FOUND: &str = " => not found";

/// Writes a shellscript
fn write_bash_script(target: &Path, script: &String) -> io::Result<()> {
    let mut file = fs::File::create(target)?;
    file.write_all(format!("#!/usr/bin/env bash\n\n{}", script).as_bytes())?;

    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o755);
    file.set_permissions(permissions)?;

    Ok(())
}

/// Returns the nix expression needed to build an appropiate FHS
fn fhs_shell<I: Iterator<Item = Package>>(run: &Path, packages: I) -> String {
    format!(
        r#"with import <nixpkgs> {{}};
  buildFHSUserEnv {{
    name = "fhs";
    targetPkgs = p: with p; [ 
      {} 
    ];
    runScript = "{}";
  }}"#,
        packages
            .map(|p| p.name)
            .collect::<Vec<_>>()
            .join("\n      "),
        run.to_str().expect("unable to stringify path")
    )
}

/// uses ldd to find missing shared object files on a given binary
fn missing_libs(binary: &Path) -> anyhow::Result<Vec<MissingLib>> {
    let output = Command::new("ldd").arg(binary.as_os_str()).output()?;

    if !output.status.success() {
        anyhow::bail!("ldd returned error code {}", output.status);
    }

    Ok(String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|l| match l.find(LDD_NOT_FOUND) {
            Some(i) => {
                let mut s = l.to_string();
                s.truncate(i);
                s.remove(0); // get rid of tabulator prefix
                Some(MissingLib {
                    name: s.trim().to_string(),
                })
            }
            None => None,
        })
        .collect())
}

/// A missing library, identified by the filename (without preceding dirnames)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MissingLib {
    name: String,
}

impl MissingLib {
    /// uses nix-locate to find candidate packages providing a given file,
    /// identified by a file name
    fn find_candidates(&self) -> anyhow::Result<Vec<Package>> {
        let db_path = dirs::home_dir()
            .ok_or_else(|| anyhow::format_err!("unable to find home dir"))?
            .join(".cache/nix-index/");
        let db = nix_index::database::Reader::open(db_path)
            .map_err(|_| anyhow::format_err!("oh no, a nix-index error"))?;
        let regex = Regex::new(&self.name)?;
        let query = db.query(&regex);
        query
            .run()
            .unwrap()
            .map(|x| {
                x.map(|p| Package {
                    name: format!("{}.{}", p.0.origin().attr, p.0.origin().output),
                })
                .map_err(|_| anyhow::format_err!("oh no, a nix-index error"))
            })
            .collect()
    }
}

/// A package providing a lib
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Package {
    name: String,
}

#[derive(Parser)]
#[clap(version, author, about)]
struct Opts {
    /// dynamically linked binary to be examined
    binary: PathBuf,

    /// additional shared object files to search for and propagate
    #[clap(short, long = "lib")]
    libs: Vec<String>,

    /// additional packages to propagate
    #[clap(short, long = "pkg")]
    pkgs: Vec<String>,

    #[clap(long)]
    print_found_packages: bool,

    #[clap(arg_enum, short, long, default_value_t)]
    output_format: Output,

    #[clap(arg_enum, short, long, default_value_t)]
    strategy: Strategy,
}

#[derive(Clone, clap::ArgEnum)]
enum Output {
    NixShell,
}

impl Default for Output {
    fn default() -> Self {
        Self::NixShell
    }
}

#[derive(Clone, clap::ArgEnum)]
enum Strategy {
    TakeAll,
}

impl Default for Strategy {
    fn default() -> Self {
        Self::TakeAll
    }
}

fn new_spinner(msg: &'static str) -> ProgressBar {
    let style = ProgressStyle::default_spinner().on_finish(ProgressFinish::AndLeave);
    ProgressBar::new_spinner()
        .with_style(style)
        .with_message(msg)
}

fn new_progress(count: u64, msg: &'static str) -> ProgressBar {
    let style = ProgressStyle::default_bar()
        .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
        .progress_chars("##-")
        .on_finish(ProgressFinish::AndLeave);
    ProgressBar::new(count).with_style(style).with_message(msg)
}

fn main() -> anyhow::Result<()> {
    let mut opts: Opts = Opts::parse();

    // initilizes packages list and adds additional-packages right away, if
    // provided

    opts.pkgs.dedup();

    let mut packages_included: Vec<_> = opts
        .pkgs
        .into_iter()
        .map(|name| Arc::new(Package { name }))
        .collect();

    let pb = new_spinner("scanning for missing libs");

    let mut missing_libs: Vec<_> = opts
        .libs
        .into_iter()
        .progress_with(pb)
        .map(|name| MissingLib { name })
        .chain(missing_libs(&opts.binary)?.into_iter())
        .collect();

    let pb = new_spinner("refining missing libs");

    packages_included.sort();
    missing_libs.sort();
    missing_libs.dedup();
    pb.finish();

    let pb = new_progress(missing_libs.len() as u64, "loooking up candidate packages");

    let missing_map: HashMap<Arc<MissingLib>, Vec<Arc<Package>>> = missing_libs
        .par_iter()
        .progress_with(pb)
        .map(|l| {
            (
                Arc::new(l.clone()),
                l.find_candidates()
                    .unwrap()
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
            )
        })
        .collect();

    let candidates_map: HashMap<Arc<Package>, Vec<Arc<MissingLib>>> =
        missing_map
            .iter()
            .fold(HashMap::new(), |mut accum, (l, ps)| {
                ps.iter()
                    .for_each(|p| accum.entry(p.clone()).or_insert(Vec::new()).push(l.clone()));
                accum
            });

    // TODO please find a good selection
    // this is the full set
    packages_included.extend(candidates_map.keys().cloned());

    if opts.print_found_packages {
        println!(
            "[ {} ]",
            packages_included
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    // build FHS expression
    let fhs_expression = fhs_shell(
        &opts.binary.canonicalize()?,
        packages_included.iter().map(|p| p.as_ref().clone()),
    );
    // write bash script with the FHS expression
    write_bash_script(
        &opts.binary.with_file_name("run-with-nix"),
        &format!("$({NIX_BUILD_FHS} '{fhs_expression}')/bin/fhs"),
    )
    .unwrap();

    Ok(())
}
