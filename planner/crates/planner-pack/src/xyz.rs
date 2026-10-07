//! 1 m terrain and surface models as XYZ text, as the German state surveys
//! deliver them: a terrain (DGM1, bare ground) and a surface (bDOM or DOM1,
//! roofs and tree crowns) in tiles named by their south-west corner in
//! kilometres of an ETRS89 UTM zone (`dgm1_33_392_5820_2_be.xyz`,
//! `dom1_33_392_5820_2_be_2026.xyz`, `dgm1_32_280_5652_1_nw.xyz`).
//!
//! A cell's clutter is surface less terrain (never below 0), from the tile
//! pairs of one source's terrain and surface. ETRS89 and WGS 84 differ by
//! well under a metre, so a tile in the pack's own zone is used as it is,
//! and one in another zone is projected into it (`build.rs`).
//!
//! Points stand at half-metre centres, 1 m apart, in any order; a line is
//! `x y z`, separated by spaces, tabs, commas or semicolons. Either half of
//! a pair may instead be a GeoTIFF of the same tile, named the same way.

use crate::PackError;
use planner_core::geo::Xy;
use planner_terrain::Grid;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

/// One source's XYZ tiles: its terrain's and its surface's, paired by the
/// corner in their names, in an ETRS89 UTM zone; and the source's name
/// and notice, for the manifest.
#[derive(Debug, Clone)]
pub struct XyzInput {
    pub terrain: Vec<PathBuf>,
    pub surface: Vec<PathBuf>,
    pub zone: u8,
    pub source: String,
    pub notice: String,
}

/// A height at or below this is a tile's no-data value.
const NO_DATA_BELOW: f32 = -1000.0;
/// An easting this large carries its zone in front (UTM eastings are below
/// a million metres).
const ZONE_PREFIXED: f64 = 1.0e7;

/// One tile of a pair, as XYZ text or as a GeoTIFF in the same zone
/// (Baden-Württemberg's terrain is XYZ and its surface GeoTIFF), by its
/// name.
pub fn read_tile(path: &Path) -> Result<Grid, PackError> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".tif") || lower.ends_with(".tiff") {
        return read_geotiff(path);
    }
    parse_xyz(std::fs::File::open(path)?)
}

/// A whole GeoTIFF as a grid, its no-data values (anything at or below
/// `NO_DATA_BELOW`, or NaN) NaN.
fn read_geotiff(path: &Path) -> Result<Grid, PackError> {
    let mut reader = planner_terrain::cog::CogReader::open(path)
        .map_err(|e| PackError::Invalid(format!("{}: {e}", path.display())))?;
    let m = *reader.meta();
    let far = Xy {
        x: m.origin.x + (m.width as f64 - 1.0) * m.dx,
        y: m.origin.y + (m.height as f64 - 1.0) * m.dy,
    };
    let min = Xy { x: m.origin.x.min(far.x), y: m.origin.y.min(far.y) };
    let max = Xy { x: m.origin.x.max(far.x), y: m.origin.y.max(far.y) };
    let mut grid = reader
        .window(min, max)
        .map_err(|e| PackError::Invalid(format!("{}: {e}", path.display())))?;
    for v in grid.data.iter_mut() {
        if !(*v > NO_DATA_BELOW) {
            *v = f32::NAN;
        }
    }
    Ok(grid)
}

/// Parse one XYZ tile into a north-up 1 m Grid.
pub fn parse_xyz<R: Read>(reader: R) -> Result<Grid, PackError> {
    let mut pts: Vec<(f64, f64, f32)> = Vec::with_capacity(4_000_000);
    let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
    let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
    for line in BufReader::new(reader).lines() {
        let line = line?;
        let mut it = line
            .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';')
            .filter(|f| !f.is_empty());
        let (Some(xs), Some(ys), Some(zs)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(x), Ok(y), Ok(z)) = (xs.parse::<f64>(), ys.parse::<f64>(), zs.parse::<f32>())
        else {
            continue;
        };
        // No ground in Germany lies 1 km below the sea: a tile's no-data
        // value (−9999), where it writes one.
        if !(z > NO_DATA_BELOW) {
            continue;
        }
        // An easting with its zone before it (Bremerhaven's 32466000.5).
        let x = if x >= ZONE_PREFIXED { x % 1.0e6 } else { x };
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        pts.push((x, y, z));
    }
    if pts.len() < 4 {
        return Err(PackError::Invalid("XYZ tile has too few points".into()));
    }
    let res = 1.0;
    let width = ((max_x - min_x) / res).round() as usize + 1;
    let height = ((max_y - min_y) / res).round() as usize + 1;
    if width * height < pts.len() {
        return Err(PackError::Invalid(format!(
            "XYZ grid inference failed: {}×{} < {} points",
            width,
            height,
            pts.len()
        )));
    }
    let mut data = vec![f32::NAN; width * height];
    for (x, y, z) in pts {
        let col = ((x - min_x) / res).round() as usize;
        let row = ((max_y - y) / res).round() as usize; // north-up
        data[row * width + col] = z;
    }
    Grid::with_axes(Xy { x: min_x, y: max_y }, res, -res, width, height, data)
        .map_err(PackError::Terrain)
}

/// A tile's south-west corner in kilometres, `"<E>_<N>"`, from its name:
/// the first easting of three digits, or of five with its zone in front,
/// followed by a northing of four (`dgm1_33_392_5820_2_be.xyz`,
/// `dgm1_32466__5925_1_hb.xyz` → `392_5820`, `466_5925`).
pub fn tile_key(file_name: &str) -> Option<String> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
    let parts: Vec<&str> = stem.split(|c| c == '_' || c == '-').filter(|p| !p.is_empty()).collect();
    let digits = |p: &str, n: usize| p.len() == n && p.bytes().all(|b| b.is_ascii_digit());
    parts
        .windows(2)
        .find(|w| (digits(w[0], 3) || digits(w[0], 5)) && digits(w[1], 4))
        .map(|w| format!("{}_{}", &w[0][w[0].len() - 3..], w[1]))
}

/// Scatter-accumulator: mean of fine samples per target-pack pixel.
/// f32 sums + u32 counts by the footprint rule — a ≤2 500-sample cell of
/// heights around 10² m loses nothing meaningful to f32.
///
/// Also tracks the subset of samples that are actually OBSTRUCTIONS (above
/// `BUILT_THRESHOLD_M`). P.1812's "representative clutter height" (§3.2.1,
/// Table 2: dense urban 20 m, urban 15 m, suburban 10 m) is the height of
/// the thing the wave diffracts over — NOT an area average that dilutes
/// roofs with the courtyards and streets between them. Measured on the
/// Berlin pack, area-averaging put central Berlin at a mean of 3.6 m
/// against a real median building height of 18.4 m, which made the city
/// propagate like open plain.
pub struct MeanAccum {
    pub sum: Vec<f32>,
    pub count: Vec<u32>,
    pub built_sum: Vec<f32>,
    pub built_count: Vec<u32>,
}

/// A sample counts as an obstruction above this height (m).
pub const BUILT_THRESHOLD_M: f32 = 2.0;

impl MeanAccum {
    pub fn new(len: usize) -> Self {
        Self {
            sum: vec![0.0; len],
            count: vec![0; len],
            built_sum: vec![0.0; len],
            built_count: vec![0; len],
        }
    }

    pub fn add(&mut self, idx: usize, v: f32) {
        self.sum[idx] += v;
        self.count[idx] += 1;
        if v > BUILT_THRESHOLD_M {
            self.built_sum[idx] += v;
            self.built_count[idx] += 1;
        }
    }

    /// Representative height for a cell: the mean over obstructing samples
    /// when the cell is meaningfully built up, else the plain mean (open
    /// ground, low vegetation).
    pub fn representative(&self, idx: usize) -> Option<f32> {
        if self.count[idx] == 0 {
            return None;
        }
        let built_fraction = self.built_count[idx] as f32 / self.count[idx] as f32;
        if built_fraction >= 0.15 {
            Some(self.built_sum[idx] / self.built_count[idx] as f32)
        } else {
            Some(self.sum[idx] / self.count[idx] as f32)
        }
    }
}

/// An input's tile pairs, `(key, terrain, surface)`, by the corner in their
/// names. A tile with no partner is said and left out.
pub fn pairs(input: &XyzInput) -> Vec<(String, PathBuf, PathBuf)> {
    let keyed = |paths: &[PathBuf]| -> HashMap<String, PathBuf> {
        paths
            .iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_str()?;
                let lower = name.to_ascii_lowercase();
                if !(lower.ends_with(".xyz") || lower.ends_with(".tif") || lower.ends_with(".tiff")) {
                    return None;
                }
                Some((tile_key(name)?, p.clone()))
            })
            .collect()
    };
    let (terrain, surface) = (keyed(&input.terrain), keyed(&input.surface));
    let mut out = Vec::new();
    for (key, t) in &terrain {
        match surface.get(key) {
            Some(s) => out.push((key.clone(), t.clone(), s.clone())),
            None => eprintln!("xyz: {}: tile {key} has a terrain but no surface, left out", input.source),
        }
    }
    for key in surface.keys().filter(|k| !terrain.contains_key(*k)) {
        eprintln!("xyz: {}: tile {key} has a surface but no terrain, left out", input.source);
    }
    out.sort();
    out
}

/// Accumulate one tile pair's terrain and clutter means onto a target pack
/// grid (given via closure world→index to keep this free of grid layout
/// assumptions).
pub fn accumulate_pair(
    dgm1_path: &Path,
    dom1_path: &Path,
    target_index: impl FnMut(f64, f64) -> Option<usize>,
    dtm_acc: &mut MeanAccum,
    clut_acc: &mut MeanAccum,
) -> Result<usize, PackError> {
    let dgm = read_tile(dgm1_path)?;
    let dom = read_tile(dom1_path)?;
    Ok(accumulate_grids(&dgm, &dom, target_index, dtm_acc, clut_acc))
}

/// Accumulation over already-parsed grids — the parallel build parses pairs
/// on worker threads and scatters serially through this.
pub fn accumulate_grids(
    dgm: &planner_terrain::Grid,
    dom: &planner_terrain::Grid,
    mut target_index: impl FnMut(f64, f64) -> Option<usize>,
    dtm_acc: &mut MeanAccum,
    clut_acc: &mut MeanAccum,
) -> usize {
    let mut added = 0;
    for row in 0..dgm.height {
        let y = dgm.origin.y + row as f64 * dgm.dy_m;
        for col in 0..dgm.width {
            let t = dgm.data[row * dgm.width + col];
            if !t.is_finite() {
                continue;
            }
            let x = dgm.origin.x + col as f64 * dgm.dx_m;
            let Some(idx) = target_index(x, y) else { continue };
            dtm_acc.add(idx, t);
            added += 1;
            if let Some(s) = dom.sample_bilinear(Xy { x, y }) {
                if s.is_finite() {
                    clut_acc.add(idx, (s - t).max(0.0));
                }
            }
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xyz_parse_orientation_and_values() {
        // 3×2 grid at half-meter centers, south-to-north order like the feed.
        let text = "\
100.5 200.5 10\n101.5 200.5 11\n102.5 200.5 12\n\
100.5 201.5 20\n101.5 201.5 21\n102.5 201.5 22\n";
        let g = parse_xyz(text.as_bytes()).unwrap();
        assert_eq!((g.width, g.height), (3, 2));
        assert_eq!(g.dy_m, -1.0);
        // Row 0 = northern row (y = 201.5).
        assert_eq!(g.data[0], 20.0);
        assert_eq!(g.data[1 * 3 + 2], 12.0);
        assert!((g.origin.y - 201.5).abs() < 1e-9);
    }

    #[test]
    fn a_tile_is_keyed_by_the_corner_in_its_name() {
        assert_eq!(tile_key("dgm1_33_392_5820_2_be.xyz"), Some("392_5820".into()));
        assert_eq!(tile_key("dom1_33_392_5820_2_be_2026.xyz"), Some("392_5820".into()));
        assert_eq!(tile_key("dgm1_32_280_5652_1_nw_2023.xyz"), Some("280_5652".into()));
        assert_eq!(tile_key("dgm1_32466__5925_1_hb.xyz"), Some("466_5925".into()));
        assert_eq!(tile_key("bdom20nc_32_425_6002_1_sh_2024.tif"), Some("425_6002".into()));
        assert_eq!(tile_key("readme.txt"), None);
    }

    #[test]
    fn terrain_and_surface_pair_by_their_corner() {
        let p = |s: &str| PathBuf::from(s);
        let input = XyzInput {
            terrain: vec![p("a/dgm1_32_280_5652_1_nw.xyz"), p("a/dgm1_32_281_5652_1_nw.xyz")],
            surface: vec![p("b/dom1_32_280_5652_1_nw.xyz"), p("b/notes.txt")],
            zone: 32,
            source: "test".into(),
            notice: String::new(),
        };
        let got = pairs(&input);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "280_5652");
        assert_eq!(got[0].2, p("b/dom1_32_280_5652_1_nw.xyz"));
    }

    #[test]
    fn a_header_a_zone_prefix_and_a_page_after_the_points_are_read_past() {
        let text = "x y z\n32100500.5 200.5 10\n32100501.5 200.5 11\n\
32100500.5 201.5 20\n32100501.5 201.5 21\n<!DOCTYPE html>\n<html></html>\n";
        let g = parse_xyz(text.as_bytes()).unwrap();
        assert_eq!((g.width, g.height), (2, 2));
        assert!((g.origin.x - 100_500.5).abs() < 1e-6, "{}", g.origin.x);
    }

    #[test]
    fn commas_separate_as_spaces_do() {
        let g = parse_xyz("100.5,200.5,10\n101.5,200.5,11\n100.5;201.5;20\n101.5 201.5 21\n".as_bytes())
            .unwrap();
        assert_eq!((g.width, g.height), (2, 2));
        assert_eq!(g.data[0], 20.0);
    }

    #[test]
    fn accumulate_builds_clutter_means() {
        let dir = std::env::temp_dir().join("planner_b1m_test");
        std::fs::create_dir_all(&dir).unwrap();
        // Terrain flat 10; surface 10 except one 4-point building at 22.
        let mut dgm = String::new();
        let mut dom = String::new();
        for y in 0..4 {
            for x in 0..4 {
                dgm.push_str(&format!("{}.5 {}.5 10\n", 100 + x, 200 + y));
                let s = if (1..3).contains(&x) && (1..3).contains(&y) { 22 } else { 10 };
                dom.push_str(&format!("{}.5 {}.5 {}\n", 100 + x, 200 + y, s));
            }
        }
        let dp = dir.join("dgm1_33_100_200_2_be.xyz");
        let sp = dir.join("dom1_33_100_200_2_be_2026.xyz");
        std::fs::write(&dp, dgm).unwrap();
        std::fs::write(&sp, dom).unwrap();

        // Single target cell covering everything.
        let mut dtm = MeanAccum::new(1);
        let mut clut = MeanAccum::new(1);
        accumulate_pair(&dp, &sp, |_, _| Some(0), &mut dtm, &mut clut).unwrap();
        assert_eq!(dtm.count[0], 16);
        assert!((dtm.sum[0] / 16.0 - 10.0).abs() < 1e-5);
        // Mean clutter = 4 building points × 12 m / 16 = 3 m.
        let c = clut.sum[0] / clut.count[0] as f32;
        assert!((c - 3.0).abs() < 0.2, "{c}");
    }
}
