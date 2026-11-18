//! Recovery after the store may have been open during an OS crash or power loss.
//!
//! With [`CrashConsistency::ProcessCrash`](crate::store::fs::options::CrashConsistency)
//! no data files are synced, so an OS crash can leave entries whose files lost
//! writes that the database already recorded.
//!
//! Recovery turns entries with owned files into partial entries without
//! bitfields, except for the external-data case below. When a partial entry
//! is loaded, its bitfield is rebuilt from what the files really hold. If
//! all of the blob survives, the entry becomes complete again.
//!
//! For external data with an owned outboard, recovery verifies the proof and
//! a referenced file in place, rebuilding a damaged proof from verified data.
//! The caller's files stay read-only.

use std::{
    fs::{self, File},
    io,
    ops::Range,
    path::{Path, PathBuf},
};

use bao_tree::{
    io::{
        outboard::PreOrderOutboard,
        sync::{valid_outboard_ranges, valid_ranges, CreateOutboard, WriteAt},
    },
    BaoTree, ChunkNum, ChunkRanges,
};
use bytes::Bytes;
use n0_error::StdResultExt;
use redb::ReadableTable;

use super::{tables::Tables, ActorError};
use crate::{
    store::{
        fs::{
            entry_state::{DataLocation, EntryState, OutboardLocation},
            import::TempPath,
            options::PathOptions,
        },
        util::size_slot,
        IROH_BLOCK_SIZE,
    },
    Hash,
};

/// What recovery did.
#[derive(Debug, Default)]
pub(super) struct Recovered {
    /// Complete entries with owned files, now partial until they are loaded.
    pub complete: usize,
    /// Partial entries, whose bitfield is rebuilt when they are loaded.
    pub partial: usize,
    /// Complete entries with external data and no owned outboard to recover.
    pub external: usize,
    /// External entries with a verified reference and usable owned outboard.
    pub external_verified: usize,
    /// External entries whose references no longer point to the recorded data.
    pub external_unavailable: usize,
}

/// Checks entries with external data and an owned outboard immediately.
/// Other entries with owned files verify their files when next loaded.
///
/// Entries that live in the database alone cannot have lost anything.
pub(super) fn recover(
    tables: &mut Tables<'_>,
    paths: &PathOptions,
) -> Result<Recovered, ActorError> {
    let mut entries = Vec::new();
    for item in tables.blobs.iter()? {
        let (hash, state) = item?;
        entries.push((hash.value(), state.value()));
    }
    let mut res = Recovered::default();
    for (hash, state) in entries {
        match state {
            EntryState::Partial { .. } => {
                remove_if_exists(&paths.bitfield_path(&hash)).std_context("removing a bitfield")?;
                res.partial += 1;
            }
            EntryState::Complete {
                data_location: DataLocation::Inline(()),
                outboard_location: OutboardLocation::Inline(()) | OutboardLocation::NotNeeded,
            } => {}
            EntryState::Complete {
                data_location: DataLocation::External(external, size),
                outboard_location: OutboardLocation::Owned,
            } => {
                let tree = BaoTree::new(size, IROH_BLOCK_SIZE);
                if let Some(external) = verify_external(paths, &hash, tree, &external)
                    .std_context("recovering external data's owned outboard")?
                {
                    tables.blobs.insert(
                        hash,
                        EntryState::Complete {
                            data_location: DataLocation::External(external, size),
                            outboard_location: OutboardLocation::Owned,
                        },
                    )?;
                    res.external_verified += 1;
                } else {
                    // Native partial storage uses owned paths only. A later
                    // explicit import can reattach the caller's file.
                    make_partial(
                        tables,
                        paths,
                        &hash,
                        size,
                        &DataLocation::External(external, size),
                        &OutboardLocation::Owned,
                    )?;
                    res.external_unavailable += 1;
                }
            }
            EntryState::Complete {
                data_location: DataLocation::External(..),
                ..
            } => res.external += 1,
            EntryState::Complete {
                data_location,
                outboard_location,
            } => {
                let size = match &data_location {
                    DataLocation::Inline(()) => inline(&tables.inline_data, &hash)?.len() as u64,
                    DataLocation::Owned(size) | DataLocation::External(_, size) => *size,
                };
                make_partial(
                    tables,
                    paths,
                    &hash,
                    size,
                    &data_location,
                    &outboard_location,
                )?;
                res.complete += 1;
            }
        }
    }
    Ok(res)
}

/// Checks the complete proof against its root before using it to check the data.
fn load_valid_outboard(
    path: &Path,
    tree: BaoTree,
    root: bao_tree::blake3::Hash,
) -> io::Result<Option<PreOrderOutboard<File>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if file.metadata()?.len() != tree.outboard_size() {
        return Ok(None);
    }
    let outboard = PreOrderOutboard {
        tree,
        root,
        data: file,
    };
    if covers_tree(tree, valid_outboard_ranges(&outboard, &ChunkRanges::all()))? {
        Ok(Some(outboard))
    } else {
        Ok(None)
    }
}

fn covers_tree(
    tree: BaoTree,
    ranges: impl IntoIterator<Item = io::Result<Range<ChunkNum>>>,
) -> io::Result<bool> {
    let mut end = ChunkNum(0);
    for range in ranges {
        let range = match range {
            Ok(range) => range,
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(err) => return Err(err),
        };
        if range.start != end {
            return Ok(false);
        }
        end = range.end;
    }
    Ok(end == tree.chunks())
}

/// Returns the usable references with a verified file first. A path that no
/// longer holds the expected bytes cannot take precedence over a valid alias.
fn verify_external(
    paths: &PathOptions,
    hash: &Hash,
    tree: BaoTree,
    external: &[PathBuf],
) -> io::Result<Option<Vec<PathBuf>>> {
    let target = paths.outboard_path(hash);
    let root = (*hash).into();
    let proof = load_valid_outboard(&target, tree, root)?;
    let mut later = Vec::new();
    let mut io_error = None;
    for (index, path) in external.iter().enumerate() {
        let result = (|| {
            let meta = match fs::metadata(path) {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(err) => return Err(err),
            };
            // Avoid opening non-file references, such as a replaced path
            // that now points to a named pipe.
            if !meta.is_file() || meta.len() != tree.size() {
                return Ok(false);
            }
            let mut data = File::open(path)?;
            if let Some(proof) = &proof {
                return covers_tree(tree, valid_ranges(proof, &data, &ChunkRanges::all()));
            }
            // Keep the caller file and the replacement proof disk-backed.
            let temp_file = paths.temp_file_name();
            let output = File::create_new(&temp_file)?;
            let temp = TempPath::new(temp_file);
            let mut proof = PreOrderOutboard {
                tree,
                root,
                data: output,
            };
            proof.init_from(&mut data)?;
            if proof.root != root {
                return Ok(false);
            }
            drop(proof);
            temp.persist(&target)?;
            Ok(true)
        })();
        match result {
            Ok(true) => {
                return Ok(Some(
                    external[index..].iter().cloned().chain(later).collect(),
                ));
            }
            Ok(false) => {}
            Err(err) => {
                // A good alias can still recover this entry. If none verifies,
                // keep the previous run and references available for retry.
                io_error.get_or_insert(err);
                later.push(path.clone());
            }
        }
    }
    io_error.map_or(Ok(None), Err)
}

/// Turns a complete entry into a partial one with the same files.
///
/// Inline parts are written to files, since a partial entry lives in files
/// only; missing files are created when it is loaded. The bitfield file is
/// removed, so the load rebuilds it from what the files really hold.
fn make_partial(
    tables: &mut Tables<'_>,
    paths: &PathOptions,
    hash: &Hash,
    size: u64,
    data_location: &DataLocation<(), u64>,
    outboard_location: &OutboardLocation,
) -> Result<(), ActorError> {
    if let DataLocation::Inline(()) = data_location {
        let data = inline(&tables.inline_data, hash)?;
        fs::write(paths.data_path(hash), data).std_context("writing data")?;
    }
    if let OutboardLocation::Inline(()) = outboard_location {
        let outboard = inline(&tables.inline_outboard, hash)?;
        fs::write(paths.outboard_path(hash), outboard).std_context("writing outboard")?;
    }
    // the size goes in the slot of the last chunk group
    let mut sizes = fs::File::create(paths.sizes_path(hash)).std_context("creating sizes")?;
    sizes
        .write_all_at(size_slot(size.saturating_sub(1)), &size.to_le_bytes())
        .std_context("writing sizes")?;
    remove_if_exists(&paths.bitfield_path(hash)).std_context("removing a bitfield")?;
    tables.inline_data.remove(hash)?;
    tables.inline_outboard.remove(hash)?;
    tables
        .blobs
        .insert(hash, EntryState::Partial { size: Some(size) })?;
    Ok(())
}

/// Reads the inline data or outboard of `hash` from `table`.
fn inline(
    table: &impl ReadableTable<Hash, &'static [u8]>,
    hash: &Hash,
) -> Result<Bytes, ActorError> {
    match table.get(hash)? {
        Some(data) => Ok(Bytes::copy_from_slice(data.value())),
        None => Err(ActorError::inconsistent(format!(
            "inline part missing for {}",
            hash.to_hex()
        ))),
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        res => res,
    }
}
