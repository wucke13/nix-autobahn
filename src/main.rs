use std::{
    collections::HashMap,
    fs,
    io::{self, prelude::*},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};

use clap::Parser;

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
    fn find_candidates(&self) -> Vec<(Package, String)> {
        let output = Command::new("nix-locate")
            .arg("--top-level")
            .arg("--type=r")
            .arg("--type=s")
            .arg("--type=x")
            .arg("--whole-name")
            .arg(&self.name)
            .output()
            .expect("failed to execute nix-locate");

        if !output.status.success() {
            panic!("nix-locate returned error code {}", output.status);
        }

        String::from_utf8(output.stdout)
            .expect("unable to parse utf8")
            .lines()
            .map(|l| {
                let begin_cut = l.find(' ').unwrap();
                let end_cut = l.match_indices('/').nth(3).unwrap().0;
                (
                    Package {
                        name: l[0..begin_cut].to_string(),
                    },
                    l[end_cut..].to_string(),
                )
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
}

fn main() -> anyhow::Result<()> {
    let mut opts: Opts = Opts::parse();

    // initilizes packages list and adds additional-packages right away, if
    // provided

    opts.pkgs.dedup();

    let mut packages_included: Vec<_> = opts
        .pkgs
        .into_iter()
        .map(|name| Rc::new(Package { name }))
        .collect();
    let mut missing_libs: Vec<_> = opts
        .libs
        .into_iter()
        .map(|name| MissingLib { name })
        .chain(missing_libs(&opts.binary)?.into_iter())
        .collect();

    packages_included.sort();
    missing_libs.sort();
    missing_libs.dedup();

    let mut candidates_map: HashMap<Rc<Package>, Vec<&MissingLib>> = HashMap::new();

    for (missing_lib, candidates) in missing_libs.iter().map(|l| (l, l.find_candidates())) {
        for (package, _file_path) in candidates {
            packages_included.push(Rc::new(package));
            let e = candidates_map
                .entry(packages_included.last().unwrap().clone())
                .or_insert(Vec::new());
            (*e).push(missing_lib);
        }
    }

    // TODO please find a good selection

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
