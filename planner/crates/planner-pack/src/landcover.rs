//! Land cover GeoTIFFs → ClutterClass layer, each read through its source's
//! own class table: ESA WorldCover's 3° tiles in degrees, NLCD's one
//! conterminous-US raster in Conus Albers.
//!
//! Classes are CATEGORICAL — sampling is nearest neighbour, never bilinear,
//! at the full image. A code the table does not name (a raster's no-data:
//! NLCD's 250, WorldCover's 0) is no class there, and the cell is left to
//! the source before it. The pack layer stores our stable
//! `ClutterClass::code()` values (per-class calibration offsets key on them).

use crate::system::{transform, System};
use crate::PackError;
pub use planner_core::profile::ClutterClass;
use planner_terrain::cog::CogReader;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

/// One land cover source: its tiles, their system (a proj string), what each
/// of its codes is, and its name and notice for the manifest.
#[derive(Debug, Clone)]
pub struct LandcoverInput {
    pub tiles: Vec<PathBuf>,
    pub proj: String,
    pub classes: Vec<(u32, ClutterClass)>,
    pub source: String,
    pub notice: String,
}

/// A source's tiles open for reading, and its table.
pub struct LandcoverTiles {
    readers: Vec<CogReader<BufReader<File>>>,
    classes: HashMap<u32, ClutterClass>,
    system: System,
}

impl LandcoverTiles {
    pub fn open(input: &LandcoverInput) -> Result<Self, PackError> {
        let mut readers = Vec::new();
        for p in &input.tiles {
            let mut r = CogReader::open(p)
                .map_err(|e| PackError::Invalid(format!("{}: {e}", p.display())))?;
            // Row-sweep access + many parallel workers: band-sized cache.
            r.tune_for_row_sweep();
            readers.push(r);
        }
        Ok(Self {
            readers,
            classes: input.classes.iter().copied().collect(),
            system: System::new(&input.proj)?,
        })
    }

    /// The class at a point of the pack's system, nearest pixel; None
    /// outside every tile or where the code is no class.
    pub fn class_at(&mut self, pack: &System, x: f64, y: f64) -> Result<Option<ClutterClass>, PackError> {
        let Some((sx, sy)) = transform(pack, &self.system, x, y) else { return Ok(None) };
        for t in self.readers.iter_mut() {
            let m = *t.meta();
            let col = ((sx - m.origin.x) / m.dx).round();
            let row = ((sy - m.origin.y) / m.dy).round();
            if col < 0.0 || row < 0.0 || col >= m.width as f64 || row >= m.height as f64 {
                continue;
            }
            let v = t.pixel(col as u32, row as u32)?;
            return Ok(self.classes.get(&(v as u32)).copied());
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// WorldCover's table, as sources.yaml gives it.
    fn worldcover(tiles: Vec<PathBuf>) -> LandcoverInput {
        use ClutterClass::*;
        LandcoverInput {
            tiles,
            proj: "+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs".into(),
            classes: vec![
                (10, Forest),
                (20, LowVegetation),
                (30, LowVegetation),
                (40, Open),
                (50, Urban),
                (60, Open),
                (70, Open),
                (80, Water),
                (90, LowVegetation),
                (95, LowVegetation),
                (100, Open),
            ],
            source: "ESA WorldCover".into(),
            notice: String::new(),
        }
    }

    /// Real-tile spot checks (skip when the tile isn't downloaded):
    /// Alexanderplatz = built-up, Großer Müggelsee = water, Grunewald =
    /// forest — three unambiguous Berlin ground truths, asked in UTM 33.
    #[test]
    fn berlin_ground_truths() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../testbed/geodata/.cache/worldcover/ESA_WorldCover_10m_2021_v200_N51E012_Map.tif");
        if !path.exists() {
            eprintln!("SKIP: WorldCover tile not downloaded ({})", path.display());
            return;
        }
        let mut wc = LandcoverTiles::open(&worldcover(vec![path])).unwrap();
        let utm = System::new("+proj=utm +zone=33 +ellps=WGS84 +datum=WGS84 +units=m +no_defs").unwrap();
        let ll = System::new("+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs").unwrap();
        let mut at = |lon: f64, lat: f64| {
            let (x, y) = transform(&ll, &utm, lon, lat).unwrap();
            wc.class_at(&utm, x, y).unwrap()
        };
        assert_eq!(at(13.4132, 52.5219), Some(ClutterClass::Urban), "Alexanderplatz");
        assert_eq!(at(13.6493, 52.4371), Some(ClutterClass::Water), "Großer Müggelsee");
        assert_eq!(at(13.2210, 52.4830), Some(ClutterClass::Forest), "Grunewald");
        // Outside the tile → None.
        assert_eq!(at(10.0, 48.85), None, "Ulm is elsewhere");
    }
}
