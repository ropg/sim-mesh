//! Zensus 2022 100 m population grid → Population layer.
//!
//! Source: destatis "Zensus2022_Bevoelkerungszahl.zip" (18.5 MB; CSV
//! `GITTER_ID_100m;x_mp_100m;y_mp_100m;Einwohner`, EPSG:3035 cell midpoints,
//! ~3.09 M populated cells; verified 2026-08-31). License dl-de/by-2-0 with
//! destatis attribution.
//!
//! Population is spread uniformly over each 100 m cell and accumulated onto
//! the pack grid as persons-per-pack-cell (approximately conserving totals).

use crate::PackError;
use proj4rs::Proj;
use std::io::{BufRead, BufReader, Read};

pub const ZENSUS_NOTICE: &str =
    "\u{a9} Statistisches Bundesamt (Destatis), Zensus 2022 \u{2014} Datenlizenz Deutschland \u{2013} Namensnennung \u{2013} Version 2.0 (dl-de/by-2-0)";

pub fn laea_proj() -> Result<Proj, PackError> {
    Proj::from_proj_string(
        "+proj=laea +lat_0=52 +lon_0=10 +x_0=4321000 +y_0=3210000 +ellps=GRS80 +units=m +no_defs",
    )
    .map_err(|e| PackError::Proj(format!("laea/EPSG:3035: {e}")))
}

/// A population grid's CSV layout: its delimiter, the header names of the
/// columns holding a cell's centre and its people, and the cell's size, in
/// the units of the grid's system.
#[derive(Debug, Clone)]
pub struct GridCsv {
    pub delimiter: char,
    pub x: String,
    pub y: String,
    pub value: String,
    pub cell_m: f64,
}

impl GridCsv {
    /// Zensus 2022's: `GITTER_ID_100m;x_mp_100m;y_mp_100m;Einwohner`.
    pub fn zensus() -> Self {
        Self {
            delimiter: ';',
            x: "x_mp_100m".into(),
            y: "y_mp_100m".into(),
            value: "Einwohner".into(),
            cell_m: 100.0,
        }
    }
}

/// Stream the CSV; for every populated cell inside the caller's region
/// (decided by `target_index`), add its population share to the pack grid.
/// `to_target` converts the grid's system → the pack CRS. The columns are
/// found by their names in the first line; a row whose value is not a
/// positive number (a suppressed cell) is passed over.
pub fn accumulate_population<R: Read>(
    reader: R,
    layout: &GridCsv,
    mut to_target: impl FnMut(f64, f64) -> Result<(f64, f64), PackError>,
    mut target_index: impl FnMut(f64, f64) -> Option<usize>,
    res_m: f64,
    // f32 by the footprint rule: persons-per-cell magnitudes lose nothing,
    // and a full-region grid halves to one layer-equivalent.
    population: &mut [f32],
) -> Result<u64, PackError> {
    let mut total_in_region = 0u64;
    let cell = layout.cell_m;
    // Sub-sample each cell finely enough that every overlapped pack cell
    // receives its share.
    let steps = ((cell / res_m).ceil() as i32).max(1);
    let mut lines = BufReader::new(reader).lines();
    let header = lines.next().transpose()?.unwrap_or_default();
    let names: Vec<&str> = header
        .split(layout.delimiter)
        .map(|h| h.trim().trim_start_matches('\u{feff}'))
        .collect();
    let col = |want: &str| {
        names.iter().position(|n| *n == want).ok_or_else(|| {
            PackError::Invalid(format!(
                "the population grid has no column {want} ({})",
                names.join(", ")
            ))
        })
    };
    let (x_col, y_col, value_col) = (col(&layout.x)?, col(&layout.y)?, col(&layout.value)?);
    let last = x_col.max(y_col).max(value_col);
    for line in lines {
        let line = line?;
        let fields: Vec<&str> = line.split(layout.delimiter).collect();
        if fields.len() <= last {
            continue;
        }
        let (Ok(x), Ok(y), Ok(pop)) = (
            fields[x_col].trim().parse::<f64>(),
            fields[y_col].trim().parse::<f64>(),
            fields[value_col].trim().parse::<f64>(),
        ) else {
            continue; // malformed rows
        };
        if pop <= 0.0 {
            continue;
        }
        if spread_cell(
            (x, y),
            (cell, cell),
            steps,
            pop,
            &mut to_target,
            &mut target_index,
            population,
        )? {
            total_in_region += pop as u64;
        }
    }
    Ok(total_in_region)
}

/// One grid cell's people, centred on `centre` and `size` wide and high in
/// its grid's units, spread uniformly over `steps` × `steps` sub-samples and
/// added to the pack cells they land in. Cheap reject first: a cell whose
/// midpoint is outside the region adds nothing and says false.
pub fn spread_cell(
    centre: (f64, f64),
    size: (f64, f64),
    steps: i32,
    pop: f64,
    to_target: &mut impl FnMut(f64, f64) -> Result<(f64, f64), PackError>,
    target_index: &mut impl FnMut(f64, f64) -> Option<usize>,
    population: &mut [f32],
) -> Result<bool, PackError> {
    let ((x, y), (w, h)) = (centre, size);
    let (tx, ty) = to_target(x, y)?;
    if target_index(tx, ty).is_none() {
        return Ok(false);
    }
    let mut hits: Vec<usize> = Vec::with_capacity((steps * steps) as usize);
    for iy in 0..steps {
        for ix in 0..steps {
            let sx = x - w / 2.0 + (ix as f64 + 0.5) * (w / steps as f64);
            let sy = y - h / 2.0 + (iy as f64 + 0.5) * (h / steps as f64);
            let (px, py) = to_target(sx, sy)?;
            if let Some(idx) = target_index(px, py) {
                hits.push(idx);
            }
        }
    }
    if !hits.is_empty() {
        // Uniform split over the sub-samples that landed in the region —
        // conserves each cell's population exactly.
        let split = (pop / hits.len() as f64) as f32;
        for idx in hits {
            population[idx] += split;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The review flagged "verify EPSG:3035 parity early" — this is that
    /// check: proj4rs must support LAEA with GRS80 well enough to place a
    /// known south-German cell midpoint in the right few-km neighborhood and
    /// round-trip to sub-meter.
    #[test]
    fn laea_projection_parity() {
        let laea = laea_proj().expect("proj4rs supports +proj=laea");
        let ll = Proj::from_proj_string("+proj=longlat +ellps=GRS80 +no_defs").unwrap();
        // First data row of the real CSV: E 4337050, N 2689150.
        let mut pt = (4337050.0, 2689150.0, 0.0);
        proj4rs::transform::transform(&laea, &ll, &mut pt).unwrap();
        let (lon, lat) = (pt.0.to_degrees(), pt.1.to_degrees());
        assert!((9.5..=11.0).contains(&lon), "lon {lon}");
        assert!((46.8..=48.2).contains(&lat), "lat {lat}");
        // Round-trip.
        let mut back = (pt.0, pt.1, 0.0);
        proj4rs::transform::transform(&ll, &laea, &mut back).unwrap();
        assert!((back.0 - 4337050.0).abs() < 0.5, "{}", back.0);
        assert!((back.1 - 2689150.0).abs() < 0.5, "{}", back.1);
    }

    #[test]
    fn population_accumulates_and_conserves() {
        // Identity "projection", 2×2 target of 100×100 m cells at res 100:
        // one zensus cell lands wholly in target cell 0.
        let csv = "GITTER_ID_100m;x_mp_100m;y_mp_100m;Einwohner\nA;50;50;40\nB;950;950;7\n";
        let mut pop = vec![0.0f32; 4];
        let total = accumulate_population(
            csv.as_bytes(),
            &GridCsv::zensus(),
            |x, y| Ok((x, y)),
            |x, y| {
                if (0.0..200.0).contains(&x) && (0.0..200.0).contains(&y) {
                    Some(((y / 100.0) as usize).min(1) * 2 + ((x / 100.0) as usize).min(1))
                } else {
                    None
                }
            },
            100.0,
            &mut pop,
        )
        .unwrap();
        assert_eq!(total, 40, "cell B is outside the region");
        assert!((pop[0] - 40.0).abs() < 1e-9, "{:?}", pop);
        assert_eq!(pop[1] + pop[2] + pop[3], 0.0);
    }

    #[test]
    fn another_layout_reads_by_its_column_names() {
        // CBS's 100 m grid as the front writes it: comma-separated, its
        // columns in another order, a suppressed cell negative.
        let csv = "value,y,x\n40,50,50\n-99997,150,150\n";
        let layout = GridCsv {
            delimiter: ',',
            x: "x".into(),
            y: "y".into(),
            value: "value".into(),
            cell_m: 100.0,
        };
        let mut pop = vec![0.0f32; 4];
        let total = accumulate_population(
            csv.as_bytes(),
            &layout,
            |x, y| Ok((x, y)),
            |x, y| {
                ((0.0..200.0).contains(&x) && (0.0..200.0).contains(&y))
                    .then(|| ((y / 100.0) as usize).min(1) * 2 + ((x / 100.0) as usize).min(1))
            },
            100.0,
            &mut pop,
        )
        .unwrap();
        assert_eq!(total, 40);
        assert!((pop[0] - 40.0).abs() < 1e-9 && pop[3] == 0.0, "{:?}", pop);
    }
}
