//! Raster grids, windowed GeoTIFF/COG reads, and profile extraction.
//!
//! Device-side stays pure Rust: `cog::CogReader` decodes only the chunks a
//! window touches (tiled or striped TIFF), so packs never decompress whole
//! rasters. Pack rasters are in a local metric CRS by contract — the pack
//! compiler does any reprojection at build time.

pub mod cog;

use planner_core::geo::Xy;
use planner_core::profile::{ClutterClass, Profile, ProfilePoint, Zone};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TerrainError {
    #[error("point ({0}, {1}) outside grid extent")]
    OutOfBounds(f64, f64),
    #[error("grid dimensions do not match data length")]
    BadDimensions,
    #[error("profile endpoints coincide")]
    DegeneratePath,
    #[error("geotiff: {0}")]
    Cog(String),
}

/// Row-major single-band raster in a local metric CRS.
///
/// `origin` is the world coordinate of the CENTER of pixel (col 0, row 0);
/// columns advance by `dx_m` (positive east), rows advance by `dy_m` — SIGNED,
/// so a north-up raster (row index growing southward) carries a negative
/// `dy_m` and needs no data flipping.
#[derive(Debug, Clone)]
pub struct Grid {
    pub origin: Xy,
    pub dx_m: f64,
    pub dy_m: f64,
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Grid {
    pub fn new(
        origin: Xy,
        res_m: f64,
        width: usize,
        height: usize,
        data: Vec<f32>,
    ) -> Result<Self, TerrainError> {
        if data.len() != width * height {
            return Err(TerrainError::BadDimensions);
        }
        Ok(Self { origin, dx_m: res_m, dy_m: res_m, width, height, data })
    }

    pub fn with_axes(
        origin: Xy,
        dx_m: f64,
        dy_m: f64,
        width: usize,
        height: usize,
        data: Vec<f32>,
    ) -> Result<Self, TerrainError> {
        if data.len() != width * height {
            return Err(TerrainError::BadDimensions);
        }
        Ok(Self { origin, dx_m, dy_m, width, height, data })
    }

    pub fn filled(origin: Xy, res_m: f64, width: usize, height: usize, value: f32) -> Self {
        Self { origin, dx_m: res_m, dy_m: res_m, width, height, data: vec![value; width * height] }
    }

    #[inline]
    fn at(&self, col: usize, row: usize) -> f32 {
        self.data[row * self.width + col]
    }

    /// Nearest-neighbour sample at a world coordinate; None outside the
    /// raster. For rasters whose values are CATEGORIES or COUNTS, where
    /// interpolating between two cells would invent a value neither of them
    /// holds — a served-by count of 0 and 2 must not average into 1.
    pub fn sample_nearest(&self, p: Xy) -> Option<f32> {
        let fx = (p.x - self.origin.x) / self.dx_m;
        let fy = (p.y - self.origin.y) / self.dy_m;
        let eps = 1e-9;
        if fx < -0.5 - eps
            || fy < -0.5 - eps
            || fx > (self.width as f64) - 0.5 + eps
            || fy > (self.height as f64) - 0.5 + eps
        {
            return None;
        }
        let c = (fx.round() as isize).clamp(0, self.width as isize - 1) as usize;
        let r = (fy.round() as isize).clamp(0, self.height as isize - 1) as usize;
        Some(self.at(c, r))
    }

    /// Bilinear sample at a world coordinate; None outside the raster. The
    /// valid domain extends half a pixel beyond the outer centers
    /// (pixel-area semantics, clamped — mirrors `cog::CogReader::sample`).
    pub fn sample_bilinear(&self, p: Xy) -> Option<f32> {
        let fx = (p.x - self.origin.x) / self.dx_m;
        let fy = (p.y - self.origin.y) / self.dy_m;
        let eps = 1e-9;
        if fx < -0.5 - eps
            || fy < -0.5 - eps
            || fx > (self.width as f64) - 0.5 + eps
            || fy > (self.height as f64) - 0.5 + eps
        {
            return None;
        }
        let fx = fx.clamp(0.0, (self.width - 1) as f64);
        let fy = fy.clamp(0.0, (self.height - 1) as f64);
        let c0 = fx.floor() as usize;
        let r0 = fy.floor() as usize;
        let c1 = (c0 + 1).min(self.width - 1);
        let r1 = (r0 + 1).min(self.height - 1);
        let tx = (fx - c0 as f64) as f32;
        let ty = (fy - r0 as f64) as f32;
        let top = self.at(c0, r0) * (1.0 - tx) + self.at(c1, r0) * tx;
        let bot = self.at(c0, r1) * (1.0 - tx) + self.at(c1, r1) * tx;
        Some(top * (1.0 - ty) + bot * ty)
    }
}

impl Grid {
    /// This grid as a [`GridView`] of itself.
    pub fn view(&self) -> GridView<'_> {
        GridView {
            origin: self.origin,
            dx_m: self.dx_m,
            dy_m: self.dy_m,
            width: self.width,
            height: self.height,
            data: &self.data,
            stride: self.width,
        }
    }

    /// The `width` × `height` cells from column `c0`, row `r0` of this grid,
    /// as the grid of their own a window read of exactly those cells returns,
    /// with its cell (0, 0) centred at `origin`. `None` where they run past
    /// this grid.
    ///
    /// `origin` is the caller's, not derived from this grid's: a window's
    /// origin is computed from the layer's own, and the same cell reached
    /// from another window's origin can differ in its last bit.
    pub fn sub_view(
        &self,
        c0: usize,
        r0: usize,
        width: usize,
        height: usize,
        origin: Xy,
    ) -> Option<GridView<'_>> {
        if width == 0 || height == 0 || c0 + width > self.width || r0 + height > self.height {
            return None;
        }
        Some(GridView {
            origin,
            dx_m: self.dx_m,
            dy_m: self.dy_m,
            width,
            height,
            data: &self.data[r0 * self.width + c0..],
            stride: self.width,
        })
    }
}

/// A window of a larger [`Grid`], read as the window's own grid: its own
/// origin and size, its cells where the larger grid holds them.
///
/// It exists so that requests answered from one shared read get the answer
/// each would have got from a read of its own: `sample_bilinear` is
/// [`Grid::sample_bilinear`] step for step, over the same cells.
#[derive(Clone, Copy, Debug)]
pub struct GridView<'a> {
    pub origin: Xy,
    pub dx_m: f64,
    pub dy_m: f64,
    pub width: usize,
    pub height: usize,
    /// From the view's cell (0, 0) on, `stride` cells to a row.
    data: &'a [f32],
    stride: usize,
}

impl GridView<'_> {
    #[inline]
    fn at(&self, col: usize, row: usize) -> f32 {
        self.data[row * self.stride + col]
    }

    /// [`Grid::sample_bilinear`] of the view's own grid, to the bit.
    pub fn sample_bilinear(&self, p: Xy) -> Option<f32> {
        let fx = (p.x - self.origin.x) / self.dx_m;
        let fy = (p.y - self.origin.y) / self.dy_m;
        let eps = 1e-9;
        if fx < -0.5 - eps
            || fy < -0.5 - eps
            || fx > (self.width as f64) - 0.5 + eps
            || fy > (self.height as f64) - 0.5 + eps
        {
            return None;
        }
        let fx = fx.clamp(0.0, (self.width - 1) as f64);
        let fy = fy.clamp(0.0, (self.height - 1) as f64);
        let c0 = fx.floor() as usize;
        let r0 = fy.floor() as usize;
        let c1 = (c0 + 1).min(self.width - 1);
        let r1 = (r0 + 1).min(self.height - 1);
        let tx = (fx - c0 as f64) as f32;
        let ty = (fy - r0 as f64) as f32;
        let top = self.at(c0, r0) * (1.0 - tx) + self.at(c1, r0) * tx;
        let bot = self.at(c0, r1) * (1.0 - tx) + self.at(c1, r1) * tx;
        Some(top * (1.0 - ty) + bot * ty)
    }
}

/// Extract a TX→RX profile over SEPARATE terrain and clutter grids (the §8.1
/// schema rule: no merged DSM inputs). Missing clutter grid ⇒ open ground.
///
/// Sampling is planar; for Berlin-scale distances in a UTM CRS this is fine.
/// Geodesic sampling for long paths comes with the pack CRS work.
pub fn extract_profile(
    terrain: &Grid,
    clutter: Option<&Grid>,
    from: Xy,
    to: Xy,
    step_m: f64,
) -> Result<Profile, TerrainError> {
    let total = from.dist_m(&to);
    if total <= 0.0 {
        return Err(TerrainError::DegeneratePath);
    }
    let n = (total / step_m).ceil() as usize;
    let n = n.max(1);
    let mut points = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let p = Xy { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t };
        let h_terrain = terrain
            .sample_bilinear(p)
            .ok_or(TerrainError::OutOfBounds(p.x, p.y))?;
        let h_clutter = match clutter {
            Some(g) => g.sample_bilinear(p).ok_or(TerrainError::OutOfBounds(p.x, p.y))?,
            None => 0.0,
        };
        points.push(ProfilePoint {
            d_m: total * t,
            h_terrain_m: h_terrain,
            h_clutter_m: h_clutter.max(0.0),
            clutter: if h_clutter > 0.5 { ClutterClass::Urban } else { ClutterClass::Open },
            zone: Zone::Inland,
        });
    }
    Ok(Profile { points })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: usize, height: usize, res: f64, value: f32) -> Grid {
        Grid::filled(Xy { x: 0.0, y: 0.0 }, res, width, height, value)
    }

    #[test]
    fn flat_profile_is_flat() {
        let g = flat(50, 3, 10.0, 42.0);
        let p = extract_profile(
            &g,
            None,
            Xy { x: 0.0, y: 10.0 },
            Xy { x: 400.0, y: 10.0 },
            100.0,
        )
        .unwrap();
        assert_eq!(p.points.len(), 5);
        assert!((p.length_m() - 400.0).abs() < 1e-9);
        assert!(p.points.iter().all(|pt| (pt.h_terrain_m - 42.0).abs() < 1e-6));
        assert!(p.points.iter().all(|pt| pt.h_clutter_m == 0.0));
    }

    #[test]
    fn hill_shows_up_mid_profile() {
        let mut g = flat(41, 3, 10.0, 0.0);
        // A ridge at column 20 (x = 200 m), all rows.
        for row in 0..3 {
            g.data[row * 41 + 20] = 100.0;
        }
        let p = extract_profile(
            &g,
            None,
            Xy { x: 0.0, y: 10.0 },
            Xy { x: 400.0, y: 10.0 },
            10.0,
        )
        .unwrap();
        let mid = &p.points[20];
        assert!((mid.d_m - 200.0).abs() < 1e-9);
        assert!((mid.h_terrain_m - 100.0).abs() < 1e-3);
        assert!(p.points[0].h_terrain_m.abs() < 1e-6);
        assert!(p.points[40].h_terrain_m.abs() < 1e-6);
    }

    #[test]
    fn clutter_layer_is_separate() {
        let terrain = flat(50, 3, 10.0, 10.0);
        let clutter = flat(50, 3, 10.0, 15.0);
        let p = extract_profile(
            &terrain,
            Some(&clutter),
            Xy { x: 0.0, y: 10.0 },
            Xy { x: 300.0, y: 10.0 },
            50.0,
        )
        .unwrap();
        assert!(p.points.iter().all(|pt| (pt.h_terrain_m - 10.0).abs() < 1e-6));
        assert!(p.points.iter().all(|pt| (pt.h_clutter_m - 15.0).abs() < 1e-6));
        let dsm: Vec<f64> = p.dsm_view().collect();
        assert!(dsm.iter().all(|h| (h - 25.0).abs() < 1e-5));
    }

    /// A view samples as the grid it stands for, to the bit: a whole grid as
    /// itself, and a sub-view as the window grid holding just its cells, at
    /// that window's own origin. Inside, on the half-cell rim, and outside.
    #[test]
    fn a_view_samples_as_the_grid_it_stands_for() {
        let (ox, oy, res) = (382_644.288_685_023_9, 5_824_457.805_585_69, 10.0);
        let (w, h) = (61usize, 47usize);
        let data: Vec<f32> = (0..w * h).map(|i| ((i as f32) * 0.618).sin() * 30.0 + 40.0).collect();
        let whole = Grid::with_axes(Xy { x: ox, y: oy }, res, -res, w, h, data).unwrap();
        // The points a path samples: off the cell grid, at awkward fractions.
        let points: Vec<Xy> = (0..4000)
            .map(|k| {
                let t = k as f64 / 3999.0;
                Xy {
                    x: ox - 9.0 + t * (w as f64 * res + 13.7),
                    y: oy + 7.0 - t * (h as f64 * res + 3.3),
                }
            })
            .collect();
        let bits = |v: Option<f32>| v.map(f32::to_bits);
        for &p in &points {
            assert_eq!(bits(whole.view().sample_bilinear(p)), bits(whole.sample_bilinear(p)));
        }
        for (c0, r0, sw, sh) in [(0, 0, w, h), (7, 3, 20, 31), (40, 30, 21, 17), (60, 46, 1, 1)] {
            // The window a read of those cells returns, origin as a reader
            // computes it from the layer's.
            let origin = Xy { x: ox + c0 as f64 * res, y: oy + r0 as f64 * -res };
            let cells: Vec<f32> = (0..sh)
                .flat_map(|r| whole.data[(r0 + r) * w + c0..(r0 + r) * w + c0 + sw].to_vec())
                .collect();
            let window = Grid::with_axes(origin, res, -res, sw, sh, cells).unwrap();
            let view = whole.sub_view(c0, r0, sw, sh, origin).unwrap();
            for &p in &points {
                assert_eq!(
                    bits(view.sample_bilinear(p)),
                    bits(window.sample_bilinear(p)),
                    "view ({c0},{r0}) {sw}x{sh} at {p:?}"
                );
            }
        }
        assert!(whole.sub_view(50, 0, 12, 1, whole.origin).is_none(), "past the right edge");
        assert!(whole.sub_view(0, 40, 1, 8, whole.origin).is_none(), "past the bottom");
        assert!(whole.sub_view(0, 0, 0, 1, whole.origin).is_none(), "no cells");
    }

    #[test]
    fn out_of_bounds_is_an_error() {
        let g = flat(10, 3, 10.0, 0.0);
        let r = extract_profile(
            &g,
            None,
            Xy { x: 0.0, y: 10.0 },
            Xy { x: 500.0, y: 10.0 },
            50.0,
        );
        assert!(r.is_err());
    }
}
