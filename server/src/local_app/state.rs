//! User's explicit pause choice survives restarts independently of config.toml.
use std::{
    fs,
    io::{ErrorKind, Write},
    path::Path,
};

use crate::Result;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    paused: bool,
}

pub(super) fn paused(path: &Path) -> Result<bool> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<State>(&bytes)?.paused),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

pub(super) fn set_paused(path: &Path, paused: bool) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().expect("state file parent"))?;
    tmp.write_all(&serde_json::to_vec(&State { paused })?)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|err| err.error)?;
    Ok(())
}
