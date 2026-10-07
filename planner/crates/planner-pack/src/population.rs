//! A population raster → Population layer: a GeoTIFF of people per pixel in
//! its own system (WorldPop's United States grid, 3 arc-seconds in degrees).
//!
//! Only the pixels whose box meets the pack grid are read. Each pixel's
//! people are spread uniformly over it and accumulated onto the pack grid as
//! persons per pack cell, as a CSV grid's cells are (`zensus::spread_cell`).
//! A pixel that is not a positive number, or is the raster's no-data, holds
//! nobody.

use crate::system::{transform, System};
use crate::zensus::spread_cell;
use crate::PackError;
use planner_terrain::cog::CogReader;
use std::path::Path;

/// Add a raster's people to `population`, the pack grid `nx` × `ny` of
/// `res`-metre cells whose first centre is `origin`, in `pack`'s system.
/// Returns how many people landed inside it.
#[allow(clippy::too_many_arguments)]
pub fn accumulate_raster(
    path: &Path,
    proj: &str,
    nodata: Option<f32>,
    pack: &System,
    origin: (f64, f64),
    res: f64,
    nx: usize,
    ny: usize,
    population: &mut [f32],
) -> Result<u64, PackError> {
    let src = System::new(proj)?;
    let mut reader =
        CogReader::open(path).map_err(|e| PackError::Invalid(format!("{}: {e}", path.display())))?;
    reader.tune_for_row_sweep();
    let m = *reader.meta();
    // The grid's outer edges, through the source's system: its corners and
    // edge midpoints, one source pixel added each side.
    let (x0, y1) = (origin.0 - res / 2.0, origin.1 + res / 2.0);
    let (x1, y0) = (x0 + nx as f64 * res, y1 - ny as f64 * res);
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    for (x, y) in [(x0, y0), (mx, y0), (x1, y0), (x1, my), (x1, y1), (mx, y1), (x0, y1), (x0, my)] {
        let (sx, sy) = transform(pack, &src, x, y)
            .ok_or_else(|| PackError::Proj("pack grid → population raster".into()))?;
        lo = (lo.0.min(sx), lo.1.min(sy));
        hi = (hi.0.max(sx), hi.1.max(sy));
    }
    let col = |x: f64| (x - m.origin.x) / m.dx;
    let row = |y: f64| (y - m.origin.y) / m.dy;
    let (ca, cb) = (col(lo.0), col(hi.0));
    let (ra, rb) = (row(lo.1), row(hi.1));
    let c0 = (ca.min(cb).floor() - 1.0).max(0.0) as u32;
    let c1 = (ca.max(cb).ceil() + 1.0).min(m.width as f64 - 1.0);
    let r0 = (ra.min(rb).floor() - 1.0).max(0.0) as u32;
    let r1 = (ra.max(rb).ceil() + 1.0).min(m.height as f64 - 1.0);
    if c1 < c0 as f64 || r1 < r0 as f64 {
        return Ok(0);
    }
    let (c1, r1) = (c1 as u32, r1 as u32);
    // Sub-samples a side: enough that every pack cell a pixel overlaps gets
    // its share.
    let pixel_m = m.dy.abs() / src.units_per_metre();
    let steps = ((pixel_m / res).ceil() as i32).max(1);
    let size = (m.dx.abs(), m.dy.abs());
    let cell_of = |x: f64, y: f64| -> Option<usize> {
        let c = ((x - origin.0) / res).round();
        let r = ((origin.1 - y) / res).round();
        (c >= 0.0 && r >= 0.0 && c < nx as f64 && r < ny as f64).then(|| r as usize * nx + c as usize)
    };
    let mut to_target = |x: f64, y: f64| {
        transform(&src, pack, x, y).ok_or_else(|| PackError::Proj("population raster → pack".into()))
    };
    let mut target_index = cell_of;
    let mut total = 0.0f64;
    for r in r0..=r1 {
        for c in c0..=c1 {
            // A chunk a sparse copy never fetched does not decode: nobody there.
            let Ok(v) = reader.pixel(c, r) else { continue };
            if !(v.is_finite() && v > 0.0) || Some(v) == nodata {
                continue;
            }
            let centre = (m.origin.x + c as f64 * m.dx, m.origin.y + r as f64 * m.dy);
            if spread_cell(centre, size, steps, v as f64, &mut to_target, &mut target_index, population)? {
                total += v as f64;
            }
        }
    }
    Ok(total.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use planner_terrain::cog::write_geotiff_f32;
    use planner_terrain::Grid;
    use planner_core::geo::Xy;

    /// A 4 × 4 raster in degrees, 0.001° pixels, round (13.4°, 52.5°), with
    /// people in two pixels and no-data in a third: what lands on a pack
    /// grid over all of it is every person, and none of the no-data.
    #[test]
    fn a_raster_in_degrees_lands_its_people_on_the_pack_grid() {
        let dir = std::env::temp_dir().join("planner_pack_population_raster");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pop.tif");
        let mut data = vec![0f32; 16];
        data[5] = 40.0;
        data[10] = 7.5;
        data[15] = -99999.0;
        let grid = Grid::with_axes(Xy { x: 13.4005, y: 52.5035 }, 0.001, -0.001, 4, 4, data).unwrap();
        write_geotiff_f32(&path, &grid).unwrap();
        let pack = System::new("+proj=utm +zone=33 +ellps=WGS84 +datum=WGS84 +units=m +no_defs").unwrap();
        let ll = System::new("+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs").unwrap();
        let (cx, cy) = transform(&ll, &pack, 13.402, 52.502).unwrap();
        let (res, n) = (30.0, 20usize);
        let origin = (cx - (n as f64 / 2.0 - 0.5) * res, cy + (n as f64 / 2.0 - 0.5) * res);
        let mut pop = vec![0f32; n * n];
        let total = accumulate_raster(
            &path,
            "+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs",
            Some(-99999.0),
            &pack,
            origin,
            res,
            n,
            n,
            &mut pop,
        )
        .unwrap();
        let landed: f32 = pop.iter().sum();
        assert_eq!(total, 48, "40 + 7.5, rounded");
        assert!((landed - 47.5).abs() < 1e-3, "{landed}");
        // The 40 are spread over the few cells under their pixel, not one.
        assert!(pop.iter().filter(|&&v| v > 0.0).count() > 4);
    }
}
