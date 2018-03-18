#![feature(slice_concat_ext)]
#![feature(type_ascription)]

extern crate flate2;
extern crate reqwest;
extern crate tar;
extern crate tempdir;

use std::collections::HashMap;
use std::convert::From;
use std::fs;
use std::io;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitStatus, Stdio};
use std::time;

use flate2::read::GzDecoder;
use tempdir::TempDir;

#[derive(Debug)]
pub enum FetchError {
  IoError(io::Error),
  RequestError(reqwest::Error),
  ParseError(reqwest::UrlError),
}

impl From<io::Error> for FetchError {
  fn from(error: io::Error) -> Self {
    FetchError::IoError(error)
  }
}

impl From<reqwest::Error> for FetchError {
  fn from(error: reqwest::Error) -> Self {
    FetchError::RequestError(error)
  }
}

impl From<reqwest::UrlError> for FetchError {
  fn from(error: reqwest::UrlError) -> Self {
    FetchError::ParseError(error)
  }
}

pub fn fetch_decompress(
  url_str: &str,
  timeout: time::Duration,
) -> Result<GzDecoder<reqwest::Response>, FetchError> {
  eprintln!("downloading .tar.gz file from '{}'...", url_str);
  let client = reqwest::Client::builder()
    .timeout(timeout)
    .gzip(true)
    .build()?;
  let parsed_url = reqwest::Url::parse(&url_str)?;
  let resp = client.get(parsed_url).send()?;
  Ok(GzDecoder::new(resp))
}

pub fn extract_into<T: Read>(stream: T, dest_dir: &Path) -> io::Result<()> {
  let mut ar = tar::Archive::new(stream);
  ar.unpack(dest_dir)
}

pub fn fetch_and_extract(
  url: &str,
  dest_dir: &Path,
  timeout: time::Duration,
) -> Result<(), FetchError> {
  let gz_stream = fetch_decompress(&url, timeout)?;
  eprintln!("extracting from response stream into {:?}...", dest_dir);
  extract_into(gz_stream, dest_dir)?;
  Ok(())
}

#[derive(Debug)]
pub enum BuildError {
  ProcessInvocationError(io::Error),
  FailedCommand(String),
}

impl From<io::Error> for BuildError {
  fn from(error: io::Error) -> Self {
    BuildError::ProcessInvocationError(error)
  }
}

fn run_command(
  exe_name_or_path: &Path,
  argv_not_first: &Vec<String>,
  cwd: &Path,
  vars: &HashMap<String, String>,
) -> Result<ExitStatus, BuildError> {
  let cmd_str: String = argv_not_first.iter().fold(
    String::from(exe_name_or_path.to_str().unwrap()),
    |cmd, arg| format!("{} {}", cmd, arg),
  );
  eprintln!("running command (in cwd {:?}) '{}'", cwd, cmd_str);
  let mut subproc: process::Child = Command::new(exe_name_or_path)
    .args(argv_not_first)
    .current_dir(cwd)
    .envs(vars)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .spawn()?;
  io::copy(&mut subproc.stdout.take().unwrap(), &mut io::stderr())?;
  let status: ExitStatus = subproc.wait()?;
  if !status.success() {
    Err(BuildError::FailedCommand(format!(
      "command '{}' failed: {}",
      cmd_str, status,
    )))
  } else {
    Ok(status)
  }
}

pub fn run_configure(
  prefix_dir: &Path,
  build_dir: &Path,
  source_dir: &Path,
  args: &Vec<String>,
  vars: &HashMap<String, String>,
) -> Result<ExitStatus, BuildError> {
  let abs_path_to_source: PathBuf = fs::canonicalize(&source_dir)?;
  eprintln!("abs_path_to_source: {:?}", abs_path_to_source);
  let configure_path: PathBuf =
    [abs_path_to_source.as_path(), Path::new("configure")]
      .iter()
      .collect();
  eprintln!("configure_path: {:?}", configure_path);
  let mut all_configure_args = args.clone();
  all_configure_args.append(&mut vec![
    String::from("--prefix"),
    String::from(prefix_dir.to_str().unwrap()),
  ]);
  Ok(run_command(
    &configure_path,
    &all_configure_args,
    &build_dir,
    &vars,
  )?)
}

pub fn run_make(
  cwd: &Path,
  args: &Vec<String>,
  vars: &HashMap<String, String>,
  parallelism: u8,
) -> Result<ExitStatus, BuildError> {
  let mut all_make_args = args.clone();
  all_make_args.insert(0, format!("-j{}", parallelism.to_string()));
  Ok(run_command(
    &Path::new("make"),
    &all_make_args,
    &cwd,
    &vars,
  )?)
}

#[derive(Debug)]
pub enum BuildAutotoolsDependencyError {
  FetchErr(FetchError),
  BuildErr(BuildError),
}

impl From<FetchError> for BuildAutotoolsDependencyError {
  fn from(error: FetchError) -> Self {
    BuildAutotoolsDependencyError::FetchErr(error)
  }
}

impl From<BuildError> for BuildAutotoolsDependencyError {
  fn from(error: BuildError) -> Self {
    BuildAutotoolsDependencyError::BuildErr(error)
  }
}

impl From<io::Error> for BuildAutotoolsDependencyError {
  fn from(error: io::Error) -> Self {
    BuildAutotoolsDependencyError::FetchErr(FetchError::IoError(error))
  }
}

pub fn build_local_autotools_dep(
  src_dir: &Path,
  build_dir: &Path,
  outdir: &Path,
  configure_args: Vec<String>,
  env_vars: HashMap<String, String>,
  parallelism: u8,
) -> Result<PathBuf, BuildAutotoolsDependencyError> {
  let src_dir_abs = fs::canonicalize(src_dir)?;
  let build_dir_abs = fs::canonicalize(build_dir)?;
  let outdir_abs = fs::canonicalize(outdir)?;

  // run configure script from source dir, in build dir, and set prefix outdir
  eprintln!("running configure...");
  run_configure(
    &outdir_abs,
    &build_dir_abs,
    &src_dir_abs,
    &configure_args,
    &env_vars,
  )?;

  // build in build dir
  eprintln!("running make...");
  run_make(&build_dir, &vec![], &env_vars, parallelism)?;

  // install to outdir
  eprintln!("running make install...");
  run_make(
    &build_dir,
    &vec![String::from("install")],
    &env_vars,
    parallelism,
  )?;

  Ok(outdir_abs)
}

pub fn fetch_build_autotools_dep(
  url: &str,
  outdir: &Path,
  src_dirname: &Path,
  configure_args: Vec<String>,
  env_vars: HashMap<String, String>,
  timeout: time::Duration,
  parallelism: u8,
) -> Result<PathBuf, BuildAutotoolsDependencyError> {
  let outdir_abs = fs::canonicalize(&outdir)?;
  let tmp_dl_dir = TempDir::new("autotools-dl")?;
  let dl_dir_abs = fs::canonicalize(tmp_dl_dir.path())?;
  eprintln!("dl_dir: {:?}", dl_dir_abs);

  fetch_and_extract(&url, dl_dir_abs.as_path(), timeout)?;
  let downloaded_source_abs = fs::canonicalize(
    [dl_dir_abs.as_path(), src_dirname].iter().collect(): PathBuf,
  )?;
  eprintln!("downloaded_source_abs: {:?}", downloaded_source_abs);

  let tmp_build_dir = TempDir::new("autotools-build")?;
  let build_dir_abs = fs::canonicalize(tmp_build_dir.path())?;
  eprintln!("build_dir_abs: {:?}", build_dir_abs);

  build_local_autotools_dep(
    downloaded_source_abs.as_path(),
    build_dir_abs.as_path(),
    outdir_abs.as_path(),
    configure_args,
    env_vars,
    parallelism,
  )
}

#[cfg(test)]
mod tests {
  #[test]
  fn it_works() {
    assert_eq!(2 + 2, 4);
  }
}
