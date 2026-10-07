//! Terrain and surface from GeoTIFFs in any coordinate system: a source's
//! bare-ground model and its surface model (AHN's DTM and DSM in RD New, at
//! 0.5 m), sampled onto the pack grid; or a bare-ground model alone (3DEP's,
//! in degrees), which gives a cell its terrain and leaves its clutter be.
//!
//! Each pack cell is sampled k × k times, k its size over the tiles' pixel
//! (at most 8 a side), each sample projected from the pack's UTM into the
//! tiles' system and read at the nearest pixel of the image whose pixel
//! suits the build (`CogReader::open_level`). A cell's terrain is the mean
//! of its terrain samples; with a surface, its clutter is the representative
//! height of surface less terrain (`MeanAccum::representative`), as Berlin's
//! 1 m pairs give theirs. `count` says how many samples had what the input
//! has (both, or the terrain); the build leaves a cell with too few to the
//! sources below it.

use crate::berlin1m::MeanAccum;
use crate::system::{transform, System};
use crate::PackError;
use planner_core::geo::Xy;
use planner_terrain::cog::{CogMeta, CogReader};
use rayon::prelude::*;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Most samples a cell takes a side.
pub const MAX_SAMPLES_SIDE: usize = 8;
/// A value past this is a raster's no-data (AHN's is the largest f32).
const NO_DATA_ABOVE: f32 = 1.0e20;
/// Readers a worker keeps open per model. A row of samples crosses a few
/// tiles at a time, and a source of 1 km tiles (Brandenburg's) is a hundred
/// of them under a 10 km pack: opened all at once by every worker, they
/// pass the 256 open files macOS allows a process.
const OPEN_PER_WORKER: usize = 8;

/// One source pair's tiles, and the system they are in (a proj string). No
/// surface tiles: the terrain stands alone.
#[derive(Debug, Clone)]
pub struct ElevationInput {
    pub terrain: Vec<PathBuf>,
    pub surface: Vec<PathBuf>,
    pub proj: String,
    /// The pixel the build reads them at, in their units (metres, or degrees
    /// for a geographic system).
    pub pixel_m: f64,
    /// The tiles' no-data value, where they have one in range (Brandenburg's
    /// −9999).
    pub nodata: Option<f32>,
    /// The source's name and notice, for the manifest.
    pub source: String,
    pub notice: String,
}

/// What a pair gave each pack cell: its terrain, its clutter (NaN for a
/// terrain alone), and how many of its samples had what the input has.
pub struct CellValues {
    pub terrain: Vec<f32>,
    pub clutter: Vec<f32>,
    pub count: Vec<u32>,
    /// Samples a cell takes: k × k.
    pub per_cell: u32,
}

fn open(path: &Path, pixel_m: f64) -> Result<CogReader<BufReader<File>>, PackError> {
    CogReader::open_level(path, pixel_m)
        .map_err(|e| PackError::Invalid(format!("{}: {e}", path.display())))
}

/// One model's tiles: each one's pixel grid, read once, and the few readers
/// a worker has open, the last one used first.
struct Tiles<'a> {
    paths: &'a [PathBuf],
    pixel_m: f64,
    nodata: Option<f32>,
    metas: Vec<CogMeta>,
    open: Vec<(usize, CogReader<BufReader<File>>)>,
}

impl<'a> Tiles<'a> {
    fn new(paths: &'a [PathBuf], pixel_m: f64, nodata: Option<f32>) -> Result<Self, PackError> {
        let metas = paths
            .iter()
            .map(|p| open(p, pixel_m).map(|r| *r.meta()))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            paths,
            pixel_m,
            nodata,
            metas,
            open: Vec::new(),
        })
    }

    /// A worker's copy: the grids, no readers yet.
    fn fresh(&self) -> Self {
        Self {
            paths: self.paths,
            pixel_m: self.pixel_m,
            nodata: self.nodata,
            metas: self.metas.clone(),
            open: Vec::new(),
        }
    }

    /// The pixel of tile `i` holding (x, y), if it holds it.
    fn pixel_of(&self, i: usize, x: f64, y: f64) -> Option<(u32, u32)> {
        let m = &self.metas[i];
        let col = ((x - m.origin.x) / m.dx).round();
        let row = ((y - m.origin.y) / m.dy).round();
        if col < 0.0 || row < 0.0 || col >= m.width as f64 || row >= m.height as f64 {
            return None;
        }
        Some((col as u32, row as u32))
    }

    /// The value of the first tile that has one at (x, y), nearest pixel.
    fn value_at(&mut self, x: f64, y: f64) -> Option<f32> {
        // The tiles open now first, the last used foremost: the next sample
        // is almost always on one of them.
        for slot in 0..self.open.len() {
            let Some((col, row)) = self.pixel_of(self.open[slot].0, x, y) else {
                continue;
            };
            self.open[..=slot].rotate_right(1);
            if let Some(v) = self.read(col, row) {
                return Some(v);
            }
        }
        for i in 0..self.metas.len() {
            if self.open.iter().any(|(j, _)| *j == i) {
                continue;
            }
            let Some((col, row)) = self.pixel_of(i, x, y) else {
                continue;
            };
            let Ok(reader) = open(&self.paths[i], self.pixel_m) else {
                continue;
            };
            if self.open.len() == OPEN_PER_WORKER {
                self.open.pop();
            }
            self.open.insert(0, (i, reader));
            if let Some(v) = self.read(col, row) {
                return Some(v);
            }
        }
        None
    }

    /// The foremost open tile's value at a pixel, if it is data.
    fn read(&mut self, col: u32, row: u32) -> Option<f32> {
        // A chunk a window never fetched does not decode: no data there.
        let v = self.open[0].1.pixel(col, row).ok()?;
        (v.is_finite() && v.abs() < NO_DATA_ABOVE && Some(v) != self.nodata).then_some(v)
    }
}

/// Sample a pair onto the pack grid: `origin` the first cell's centre, `res`
/// the cell, `pack` the pack's projection.
pub fn sample(
    input: &ElevationInput,
    origin: Xy,
    res: f64,
    nx: usize,
    ny: usize,
    pack: &System,
) -> Result<CellValues, PackError> {
    let src = System::new(&input.proj)?;
    let alone = input.surface.is_empty();
    // Every tile opened once for its grid; each worker opens its own readers.
    let terrain_tiles = Tiles::new(&input.terrain, input.pixel_m, input.nodata)?;
    let surface_tiles = Tiles::new(&input.surface, input.pixel_m, input.nodata)?;
    let pixel = terrain_tiles
        .metas
        .first()
        .map(|m| m.dy.abs() / src.units_per_metre())
        .unwrap_or(res);
    let k = ((res / pixel).round() as usize).clamp(1, MAX_SAMPLES_SIDE);
    let mut terrain = vec![f32::NAN; nx * ny];
    let mut clutter = vec![f32::NAN; nx * ny];
    let mut count = vec![0u32; nx * ny];
    terrain
        .par_chunks_mut(nx)
        .zip(clutter.par_chunks_mut(nx))
        .zip(count.par_chunks_mut(nx))
        .enumerate()
        .for_each_init(
            || (terrain_tiles.fresh(), surface_tiles.fresh()),
            |(ter, sur), (row, ((t_row, c_row), n_row))| {
                let cy = origin.y - row as f64 * res;
                let mut above = MeanAccum::new(nx);
                for col in 0..nx {
                    let cx = origin.x + col as f64 * res;
                    let mut t_sum = 0.0f32;
                    let mut t_n = 0u32;
                    for iy in 0..k {
                        for ix in 0..k {
                            let x = cx - res / 2.0 + (ix as f64 + 0.5) * res / k as f64;
                            let y = cy + res / 2.0 - (iy as f64 + 0.5) * res / k as f64;
                            let Some((sx, sy)) = transform(pack, &src, x, y) else {
                                continue;
                            };
                            let Some(t) = ter.value_at(sx, sy) else { continue };
                            if !alone {
                                let Some(s) = sur.value_at(sx, sy) else { continue };
                                above.add(col, (s - t).max(0.0));
                            }
                            t_sum += t;
                            t_n += 1;
                        }
                    }
                    if t_n == 0 {
                        continue;
                    }
                    if !alone {
                        let Some(clutter) = above.representative(col) else {
                            continue;
                        };
                        c_row[col] = clutter;
                    }
                    t_row[col] = t_sum / t_n as f32;
                    n_row[col] = t_n;
                }
            },
        );
    Ok(CellValues {
        terrain,
        clutter,
        count,
        per_cell: (k * k) as u32,
    })
}
