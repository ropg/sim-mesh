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

use crate::xyz::MeanAccum;
use crate::system::{transform, System};
use crate::PackError;
use planner_core::geo::Xy;
use planner_terrain::cog::{CogMeta, CogReader};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Most samples a cell takes a side.
pub const MAX_SAMPLES_SIDE: usize = 8;
/// A value past this is a raster's no-data (AHN's is the largest f32).
const NO_DATA_ABOVE: f32 = 1.0e20;
/// Files the sampler keeps open, all its workers together: half the 256 a
/// macOS process may open by default. A row of samples crosses a few tiles
/// at a time, and a source of 1 km tiles (Brandenburg's) is a hundred of
/// them under a 10 km pack, so a worker keeps only the last few it read.
const OPEN_FILES: usize = 128;

/// Readers a worker keeps open per model, its share of `OPEN_FILES`: a
/// reader holds a file, or two for an uncompressed strip image (`cog.rs`'s
/// direct rows), and a worker reads two models. At most eight, and at least
/// two, one either side of a tile's edge, so that past 16 workers the share
/// is exceeded rather than a cell's samples reopening tiles in turn.
fn open_per_worker() -> usize {
    (OPEN_FILES / (4 * rayon::current_num_threads())).clamp(2, 8)
}

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
    /// The values the tiles write where they have no data, where those are
    /// in range (Brandenburg's −9999); one source's may differ from the
    /// other's.
    pub nodata: Vec<f32>,
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

/// One model's tiles as every worker sees them: each one's pixel grid,
/// read once, and an index of the tiles meeting each square of the largest
/// tile's size, so a sample tries the one or few tiles under it rather
/// than every tile of the source.
struct Grids<'a> {
    paths: &'a [PathBuf],
    pixel_m: f64,
    nodata: &'a [f32],
    metas: Vec<CogMeta>,
    /// The index's square, in the tiles' units.
    side: f64,
    index: HashMap<(i64, i64), Vec<usize>>,
}

impl<'a> Grids<'a> {
    fn new(paths: &'a [PathBuf], pixel_m: f64, nodata: &'a [f32]) -> Result<Self, PackError> {
        let metas: Vec<CogMeta> = paths
            .iter()
            .map(|p| open(p, pixel_m).map(|r| *r.meta()))
            .collect::<Result<_, _>>()?;
        let extents: Vec<[f64; 4]> = metas.iter().map(extent).collect();
        let side = extents
            .iter()
            .map(|e| (e[2] - e[0]).max(e[3] - e[1]))
            .fold(0.0, f64::max);
        let side = if side > 0.0 { side } else { 1.0 };
        let square = |x: f64, y: f64| ((x / side).floor() as i64, (y / side).floor() as i64);
        let mut index: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, e) in extents.iter().enumerate() {
            let ((x0, y0), (x1, y1)) = (square(e[0], e[1]), square(e[2], e[3]));
            for sx in x0..=x1 {
                for sy in y0..=y1 {
                    index.entry((sx, sy)).or_default().push(i);
                }
            }
        }
        Ok(Self {
            paths,
            pixel_m,
            nodata,
            metas,
            side,
            index,
        })
    }

    /// The tiles that may hold (x, y), in the order given.
    fn under(&self, x: f64, y: f64) -> &[usize] {
        let square = (
            (x / self.side).floor() as i64,
            (y / self.side).floor() as i64,
        );
        self.index.get(&square).map_or(&[], Vec::as_slice)
    }

    /// The pixel of tile `i` holding (x, y), if it holds it: the nearest
    /// pixel centre, a point on the edge two tiles share in the one east or
    /// south of it.
    fn pixel_of(&self, i: usize, x: f64, y: f64) -> Option<(u32, u32)> {
        let m = &self.metas[i];
        let col = ((x - m.origin.x) / m.dx + 0.5).floor();
        let row = ((y - m.origin.y) / m.dy + 0.5).floor();
        // Written to fail for NaN, which no pixel holds.
        if !(col >= 0.0 && row >= 0.0 && col < m.width as f64 && row < m.height as f64) {
            return None;
        }
        Some((col as u32, row as u32))
    }
}

/// The outer edges `[min_x, min_y, max_x, max_y]` of an image's pixels.
fn extent(m: &CogMeta) -> [f64; 4] {
    let (x0, y0) = (m.origin.x - 0.5 * m.dx, m.origin.y - 0.5 * m.dy);
    let (x1, y1) = (x0 + m.width as f64 * m.dx, y0 + m.height as f64 * m.dy);
    [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]
}

/// A worker's readers of one model's tiles: the few it has open, the last
/// one used first.
struct Tiles<'a> {
    grids: &'a Grids<'a>,
    open: Vec<(usize, CogReader<BufReader<File>>)>,
    /// Readers kept open at most.
    cap: usize,
}

impl<'a> Tiles<'a> {
    fn new(grids: &'a Grids<'a>) -> Self {
        Self {
            grids,
            open: Vec::new(),
            cap: open_per_worker(),
        }
    }

    /// The value of the first tile, in the order given, that has one at
    /// (x, y), nearest pixel. A tile that holds (x, y) and will not open is
    /// an error, not a hole: a build that ran out of open files would
    /// otherwise lose its measurements without a word.
    fn value_at(&mut self, x: f64, y: f64) -> Result<Option<f32>, PackError> {
        let grids = self.grids;
        for &i in grids.under(x, y) {
            let Some((col, row)) = grids.pixel_of(i, x, y) else {
                continue;
            };
            // A chunk a window never fetched does not decode: no data there.
            let Ok(v) = self.reader(i)?.pixel(col, row) else {
                continue;
            };
            if v.is_finite() && v.abs() < NO_DATA_ABOVE && !grids.nodata.contains(&v) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// Tile `i`'s reader, now foremost: the one open, or one opened in place
    /// of the one used longest ago.
    fn reader(&mut self, i: usize) -> Result<&mut CogReader<BufReader<File>>, PackError> {
        match self.open.iter().position(|(j, _)| *j == i) {
            Some(slot) => self.open[..=slot].rotate_right(1),
            None => {
                let reader = open(&self.grids.paths[i], self.grids.pixel_m)?;
                self.open.truncate(self.cap - 1);
                self.open.insert(0, (i, reader));
            }
        }
        Ok(&mut self.open[0].1)
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
    let terrain_grids = Grids::new(&input.terrain, input.pixel_m, &input.nodata)?;
    let surface_grids = Grids::new(&input.surface, input.pixel_m, &input.nodata)?;
    let pixel = terrain_grids
        .metas
        .first()
        .map(|m| m.dy.abs() / src.units_per_metre())
        .unwrap_or(res);
    let k = ((res / pixel).round() as usize).clamp(1, MAX_SAMPLES_SIDE);
    let mut terrain = vec![f32::NAN; nx * ny];
    let mut clutter = vec![f32::NAN; nx * ny];
    let mut count = vec![0u32; nx * ny];
    // The first error any worker meets ends the sampling and is the build's.
    terrain
        .par_chunks_mut(nx)
        .zip(clutter.par_chunks_mut(nx))
        .zip(count.par_chunks_mut(nx))
        .enumerate()
        .try_for_each_init(
            || (Tiles::new(&terrain_grids), Tiles::new(&surface_grids)),
            |(ter, sur), (row, ((t_row, c_row), n_row))| -> Result<(), PackError> {
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
                            let Some(t) = ter.value_at(sx, sy)? else { continue };
                            if !alone {
                                let Some(s) = sur.value_at(sx, sy)? else { continue };
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
                Ok(())
            },
        )?;
    Ok(CellValues {
        terrain,
        clutter,
        count,
        per_cell: (k * k) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use planner_terrain::cog::write_geotiff_f32;
    use planner_terrain::Grid;

    /// The pack's system, and the test tiles' too.
    const UTM33: &str = "+proj=utm +zone=33 +ellps=WGS84 +datum=WGS84 +units=m +no_defs";

    /// A 300 m tile of 10 m pixels, its south-west corner at (x0, y0), each
    /// pixel `v` but a no-data −9999 at `hole` (column, row).
    fn tile(path: &Path, x0: f64, y0: f64, v: f32, hole: Option<(usize, usize)>) {
        let mut data = vec![v; 30 * 30];
        if let Some((c, r)) = hole {
            data[r * 30 + c] = -9999.0;
        }
        let origin = Xy {
            x: x0 + 5.0,
            y: y0 + 295.0,
        };
        let g = Grid::with_axes(origin, 10.0, -10.0, 30, 30, data).unwrap();
        write_geotiff_f32(path, &g).unwrap();
    }

    /// Twelve tiles across, more than a worker keeps open, and two down:
    /// every pack cell they cover is measured by all nine of its samples,
    /// but the one cell with a sample on a no-data pixel, by eight. A tile
    /// that will not open is the build's error, not a hole in it.
    #[test]
    fn elevation_sample_keeps_every_tile() {
        let dir = std::env::temp_dir().join(format!("planner_elevation_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut terrain, mut surface) = (Vec::new(), Vec::new());
        for j in 0..2 {
            for i in 0..12 {
                let (x0, y0) = (300_000.0 + 300.0 * i as f64, 5_800_000.0 + 300.0 * j as f64);
                let (t, s) = (
                    dir.join(format!("t_{i}_{j}.tif")),
                    dir.join(format!("s_{i}_{j}.tif")),
                );
                tile(&t, x0, y0, 30.0, ((i, j) == (5, 1)).then_some((3, 4)));
                tile(&s, x0, y0, 40.0, None);
                terrain.push(t);
                surface.push(s);
            }
        }
        let input = ElevationInput {
            terrain,
            surface,
            proj: UTM33.into(),
            pixel_m: 7.5,
            nodata: vec![-9999.0],
            source: "test".into(),
            notice: "test".into(),
        };
        let pack = System::new(UTM33).unwrap();
        // 30 m cells over the 3.6 × 0.6 km the tiles cover, 3 × 3 samples each.
        let got = sample(
            &input,
            Xy {
                x: 300_015.0,
                y: 5_800_585.0,
            },
            30.0,
            120,
            20,
            &pack,
        )
        .unwrap();
        assert_eq!(got.per_cell, 9);
        // The no-data pixel's centre (301_535, 5_800_555) is in row 1, column 51.
        let hole = 120 + 51;
        for c in 0..120 * 20 {
            assert_eq!(got.count[c], if c == hole { 8 } else { 9 }, "cell {c}");
            assert_eq!((got.terrain[c], got.clutter[c]), (30.0, 10.0), "cell {c}");
        }

        let grids = Grids::new(&input.terrain, 7.5, &[]).unwrap();
        let mut tiles = Tiles::new(&grids);
        std::fs::remove_file(&input.terrain[7]).unwrap();
        let err = tiles
            .value_at(302_150.0, 5_800_150.0)
            .unwrap_err()
            .to_string();
        assert!(err.contains("t_7_0.tif"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two 300 m tiles and a 600 m one beside them, indexed by squares of
    /// the largest: every point is found in the tile that holds it and in no
    /// other, a point on the edge two tiles share in the one east of it,
    /// and NaN in none.
    #[test]
    fn the_index_finds_each_point_in_the_one_tile_holding_it() {
        let dir = std::env::temp_dir().join(format!("planner_index_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = Vec::new();
        for (i, (x0, side)) in [(300_000.0, 30), (300_300.0, 30), (300_600.0, 60)]
            .into_iter()
            .enumerate()
        {
            let path = dir.join(format!("{i}.tif"));
            let origin = Xy {
                x: x0 + 5.0,
                y: 5_800_000.0 + 10.0 * side as f64 - 5.0,
            };
            let g =
                Grid::with_axes(origin, 10.0, -10.0, side, side, vec![1.0; side * side]).unwrap();
            write_geotiff_f32(&path, &g).unwrap();
            paths.push(path);
        }
        let grids = Grids::new(&paths, 7.5, &[]).unwrap();
        assert_eq!(grids.side, 600.0);
        let holding = |x: f64, y: f64| -> Vec<usize> {
            let under = grids.under(x, y).iter().copied();
            under
                .filter(|&i| grids.pixel_of(i, x, y).is_some())
                .collect()
        };
        for ix in 0..120 {
            for iy in 0..60 {
                let (x, y) = (300_005.0 + 10.0 * ix as f64, 5_800_005.0 + 10.0 * iy as f64);
                let want: Vec<usize> = match (ix, iy) {
                    (0..30, 0..30) => vec![0],
                    (30..60, 0..30) => vec![1],
                    (60.., _) => vec![2],
                    _ => vec![],
                };
                assert_eq!(holding(x, y), want, "({x}, {y})");
            }
        }
        assert_eq!(holding(300_300.0, 5_800_100.0), vec![1]);
        assert_eq!(holding(300_600.0, 5_800_100.0), vec![2]);
        assert_eq!(holding(f64::NAN, 5_800_100.0), Vec::<usize>::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
