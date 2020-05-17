use std::fs;
use std::io::{self, prelude::*};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Clap;
use console::Style;
use dialoguer::{theme::ColorfulTheme, Select};
use futures::prelude::*;
use smol::blocking;

const NIX_BUILD_FHS: &'static str = "nix-build --no-out-link -E";
const LDD_NOT_FOUND: &'static str = " => not found";

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
fn fhs_shell(run: &Path, packages: Vec<String>) -> String {
    format!(
        r#"with import <nixpkgs> {{}};
  buildFHSUserEnv {{
    name = "fhs";
    targetPkgs = p: with p; [ 
      {} 
    ];
    runScript = "{}";
  }}"#,
        packages.join("\n      "),
        run.to_str().expect("unable to stringify path")
    )
}

/// uses ldd to find missing shared object files on a given binary
fn missing_libs(binary: &Path) -> Vec<String> {
    let output = Command::new("ldd")
        .arg(binary.to_str().expect("unable to stringify path"))
        .output()
        .expect("failed to execute ldd");

    if !output.status.success() {
        panic!("ldd returned error code {}", output.status);
    }

    String::from_utf8(output.stdout)
        .expect("unable to parse utf8")
        .lines()
        .filter_map(|l| match l.find(LDD_NOT_FOUND) {
            Some(i) => {
                let mut s = l.to_string();
                s.truncate(i);
                s.remove(0); // get rid of tabulator prefix
                Some(s.trim().to_string())
            }
            None => None,
        })
        .collect()
}

/// uses nix-locate to find candidate packages providing a given file,
/// identified by a file name
fn find_candidates(file_name: &String) -> Vec<(String, String)> {
    let output = Command::new("nix-locate")
        .arg("--top-level")
        .arg("--type=r")
        .arg("--type=s")
        .arg("--type=x")
        .arg("--whole-name")
        .arg(file_name)
        .output()
        .expect("failed to execute nix-locate");

    if !output.status.success() {
        panic!("nix-locate returned error code {}", output.status);
    }

    String::from_utf8(output.stdout)
        .expect("unable to parse utf8")
        .lines()
        .map(|l| {
            let begin_cut = l.find(" ").unwrap();
            let end_cut = l.match_indices("/").skip(3).nth(0).unwrap().0;
            (l[0..begin_cut].to_string(), l[end_cut..].to_string())
        })
        .collect()
}

#[derive(Clap)]
#[clap(version, author, about)]
struct Opts {
    /// dynamically linked binary to be examined
    #[clap()]
    binary: PathBuf,

    /// additional shared object files to search for and propagate
    #[clap(short = "l", long = "lib")]
    libs: Vec<String>,

    /// additional packages to propagate
    #[clap(short = "p", long = "pkgs")]
    pkgs: Vec<String>,
}

fn main() {
    let mut opts: Opts = Opts::parse();

    // initilizes packages list and adds additional-packages right away, if
    // provided

    opts.pkgs.dedup();
    opts.pkgs.sort();

    opts.libs.dedup();
    opts.libs.sort();

    smol::run(async {
        let candidates_stream = opts
            .libs
            .clone()
            .into_iter()
            .map(|string| find_candidates(&string))
            .enumerate();
        let mut candidates_stream = smol::iter(blocking!(candidates_stream));

        while let Some((i, candidates)) = candidates_stream.next().await {
            let lib = &opts.libs[i];
            match candidates.len() {
                0 => panic!("Found no provide for {}", lib),
                1 => opts.pkgs.push(candidates[0].0.clone()),
                _ if candidates.iter().any(|c| opts.pkgs.contains(&c.0)) => {}
                _ => {
                    let bold = Style::new().bold().red();
                    let selections: Vec<String> = candidates
                        .iter()
                        .map(|c| format!("{} {}", c.0, c.1))
                        .collect();
                    let choice = Select::with_theme(&ColorfulTheme::default())
                        .with_prompt(&format!("Pick provider for {}", bold.apply_to(lib)))
                        .default(0)
                        .items(&selections[..])
                        .interact()
                        .unwrap();
                    opts.pkgs.push(candidates[choice].0.clone());
                }
            }
        }

        // build FHS expression
        let fhs_expression = fhs_shell(&opts.binary.canonicalize().unwrap(), opts.pkgs);
        // write bash script with the FHS expression
        write_bash_script(
            &opts.binary.with_file_name("run-with-nix"),
            &format!("$({} '{}')/bin/fhs", NIX_BUILD_FHS, fhs_expression),
        )
        .unwrap();
    });
}
