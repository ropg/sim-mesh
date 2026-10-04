//! Terrain and surface from GeoTIFFs in any coordinate system: a source's
//! bare-ground model and its surface model (AHN's DTM and DSM in RD New, at
//! 0.5 m), sampled onto the pack grid.
//!
//! Each pack cell is sampled k × k times, k its size over the tiles' pixel
//! (at most 8 a side), each sample projected from the pack's UTM into the
//! tiles' system and read at the nearest pixel of the image whose pixel
//! suits the build (`CogReader::open_level`). A cell's terrain is the mean
//! of its terrain samples; its clutter is the representative height of
//! surface less terrain (`MeanAccum::representative`), as Berlin's 1 m pairs
//! give theirs. `count` says how many samples had both; the build leaves a
//! cell with too few to the sources below it.

use crate::berlin1m::MeanAccum;
use crate::PackError;
use planner_core::geo::Xy;
use planner_terrain::cog::CogReader;
use proj4rs::Proj;
use rayon::prelude::*;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

type Tiles = Vec<CogReader<BufReader<File>>>;

/// Most samples a cell takes a side.
pub const MAX_SAMPLES_SIDE: usize = 8;
/// A value past this is a raster's no-data (AHN's is the largest f32).
const NO_DATA_ABOVE: f32 = 1.0e20;

/// One source pair's tiles, and the system they are in (a proj string).
#[derive(Debug, Clone)]
pub struct ElevationInput {
    pub terrain: Vec<PathBuf>,
    pub surface: Vec<PathBuf>,
    pub proj: String,
    /// The pixel the build reads them at, in their units (metres).
    pub pixel_m: f64,
    /// The source's name and notice, for the manifest.
    pub source: String,
    pub notice: String,
}

/// What a pair gave each pack cell: its terrain, its clutter, and how many
/// of its samples both models had.
pub struct CellValues {
    pub terrain: Vec<f32>,
    pub clutter: Vec<f32>,
    pub count: Vec<u32>,
    /// Samples a cell takes: k × k.
    pub per_cell: u32,
}

fn open_all(paths: &[PathBuf], pixel_m: f64) -> Result<Tiles, PackError> {
    paths
        .iter()
        .map(|p| {
            CogReader::open_level(p, pixel_m)
                .map_err(|e| PackError::Invalid(format!("{}: {e}", p.display())))
        })
        .collect()
}

/// The value of the first tile that has one at (x, y), nearest pixel.
fn value_at(tiles: &mut [CogReader<BufReader<File>>], x: f64, y: f64) -> Option<f32> {
    for t in tiles.iter_mut() {
        let m = *t.meta();
        let col = ((x - m.origin.x) / m.dx).round();
        let row = ((y - m.origin.y) / m.dy).round();
        if col < 0.0 || row < 0.0 || col >= m.width as f64 || row >= m.height as f64 {
            continue;
        }
        // A chunk a window never fetched does not decode: no data there.
        match t.pixel(col as u32, row as u32) {
            Ok(v) if v.is_finite() && v.abs() < NO_DATA_ABOVE => return Some(v),
            _ => continue,
        }
    }
    None
}

/// Sample a pair onto the pack grid: `origin` the first cell's centre, `res`
/// the cell, `pack` the pack's projection.
pub fn sample(
    input: &ElevationInput,
    origin: Xy,
    res: f64,
    nx: usize,
    ny: usize,
    pack: &Proj,
) -> Result<CellValues, PackError> {
    let src = Proj::from_proj_string(&input.proj)
        .map_err(|e| PackError::Proj(format!("{}: {e}", input.proj)))?;
    // Validate once; each worker opens its own readers.
    let first = open_all(&input.terrain, input.pixel_m)?;
    open_all(&input.surface, input.pixel_m)?;
    let pixel = first.first().map(|t| t.meta().dx.abs()).unwrap_or(res);
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
            || {
                (
                    open_all(&input.terrain, input.pixel_m).expect("validated above"),
                    open_all(&input.surface, input.pixel_m).expect("validated above"),
                )
            },
            |(ter, sur), (row, ((t_row, c_row), n_row))| {
                let cy = origin.y - row as f64 * res;
                let mut above = MeanAccum::new(nx);
                for col in 0..nx {
                    let cx = origin.x + col as f64 * res;
                    let mut t_sum = 0.0f32;
                    for iy in 0..k {
                        for ix in 0..k {
                            let x = cx - res / 2.0 + (ix as f64 + 0.5) * res / k as f64;
                            let y = cy + res / 2.0 - (iy as f64 + 0.5) * res / k as f64;
                            let mut p = (x, y, 0.0);
                            if proj4rs::transform::transform(pack, &src, &mut p).is_err() {
                                continue;
                            }
                            let (Some(t), Some(s)) =
                                (value_at(ter, p.0, p.1), value_at(sur, p.0, p.1))
                            else {
                                continue;
                            };
                            t_sum += t;
                            above.add(col, (s - t).max(0.0));
                        }
                    }
                    let Some(clutter) = above.representative(col) else {
                        continue;
                    };
                    let n = above.count[col];
                    t_row[col] = t_sum / n as f32;
                    c_row[col] = clutter;
                    n_row[col] = n;
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
