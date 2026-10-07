//! A point from one coordinate system into another, each in its own units:
//! metres for a projected system, degrees for a geographic one (proj4rs
//! itself takes and gives radians there). A source's GeoTIFF is georeferenced
//! in its units, so what samples it asks in them.

use crate::PackError;
use proj4rs::Proj;

/// One system: its projection, and whether it counts in degrees.
pub struct System {
    pub proj: Proj,
    pub degrees: bool,
}

impl System {
    pub fn new(proj: &str) -> Result<Self, PackError> {
        let proj = Proj::from_proj_string(proj).map_err(|e| PackError::Proj(format!("{proj}: {e}")))?;
        let degrees = proj.is_latlong();
        Ok(Self { proj, degrees })
    }

    /// A distance in metres in this system's units, near enough for a
    /// pixel's size or a margin: a degree of latitude is 111.32 km.
    pub fn units_per_metre(&self) -> f64 {
        if self.degrees {
            1.0 / M_PER_DEGREE
        } else {
            1.0
        }
    }
}

pub const M_PER_DEGREE: f64 = 111_320.0;

/// (x, y) in `from`'s units as `to`'s, or `None` where the projection fails.
pub fn transform(from: &System, to: &System, x: f64, y: f64) -> Option<(f64, f64)> {
    let mut p = if from.degrees { (x.to_radians(), y.to_radians(), 0.0) } else { (x, y, 0.0) };
    proj4rs::transform::transform(&from.proj, &to.proj, &mut p).ok()?;
    Some(if to.degrees { (p.0.to_degrees(), p.1.to_degrees()) } else { (p.0, p.1) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degrees_go_in_and_come_out_as_degrees() {
        let utm = System::new("+proj=utm +zone=10 +ellps=WGS84 +datum=WGS84 +units=m +no_defs").unwrap();
        let nad83 = System::new("+proj=longlat +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +no_defs").unwrap();
        assert!(nad83.degrees && !utm.degrees);
        let (x, y) = transform(&nad83, &utm, -122.0, 37.5).unwrap();
        assert!((x - 588_396.0).abs() < 50.0 && (y - 4_150_850.0).abs() < 200.0, "{x} {y}");
        let (lon, lat) = transform(&utm, &nad83, x, y).unwrap();
        assert!((lon + 122.0).abs() < 1e-7 && (lat - 37.5).abs() < 1e-7, "{lon} {lat}");
    }
}
