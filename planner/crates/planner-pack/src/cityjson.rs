//! CityJSON buildings → `buildings.jsonl` records, as LoD2's CityGML gives
//! them (`lod2.rs`): 3DBAG's tiles, and any CityJSON whose buildings carry a
//! footprint and their ground and roof heights.
//!
//! A building's footprint is its LoD 0 surface, or its parts'; its ground
//! and roof heights are the attributes the source names (3DBAG's
//! `b3_h_maaiveld` and `b3_h_dak_70p`), both above the same datum, so the
//! building's height is their difference. Vertices are the file's integers
//! under its `transform`, in the source's system, which `to_pack` takes to
//! the pack's. A building without a footprint, without either height, or no
//! taller than its ground is left out and counted.
//!
//! Verified 2026-10-04 against 3DBAG v20250903's Delft tiles: CityJSON 2.0
//! in EPSG:7415, each `Building` with its LoD 0 MultiSurface.

use crate::lod2::{footprint, Lod2Building, Polygon};
use crate::PackError;
use planner_buildings::HeightSource;
use serde_json::Value;
use std::io::Read;

/// Which attributes give a building's ground and roof height.
#[derive(Debug, Clone)]
pub struct Heights {
    pub ground: String,
    pub roof: String,
}

fn number(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// The LoD 0 surfaces of an object as rings of vertex indices: each surface
/// [outer, holes…].
fn lod0_surfaces(obj: &Value) -> Vec<Vec<Vec<usize>>> {
    let mut out = Vec::new();
    for g in obj
        .get("geometry")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let lod = g.get("lod").map(|l| match l {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
        if lod.as_deref() != Some("0") {
            continue;
        }
        let Some(boundaries) = g.get("boundaries").and_then(Value::as_array) else {
            continue;
        };
        // MultiSurface: [surface [ring [index]]]
        for surface in boundaries {
            let rings: Vec<Vec<usize>> = surface
                .as_array()
                .into_iter()
                .flatten()
                .map(|ring| {
                    ring.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|i| i.as_u64().map(|i| i as usize))
                        .collect()
                })
                .collect();
            if !rings.is_empty() {
                out.push(rings);
            }
        }
    }
    out
}

/// Every building of one CityJSON document; and how many were left out.
pub fn parse_cityjson<R: Read>(
    reader: R,
    heights: &Heights,
    mut to_pack: impl FnMut(f64, f64) -> Option<(f64, f64)>,
) -> Result<(Vec<Lod2Building>, usize), PackError> {
    let doc: Value = serde_json::from_reader(reader)
        .map_err(|e| PackError::Invalid(format!("not CityJSON: {e}")))?;
    // E and N of the transform, each its default when the file has none.
    let transform = |k: &str, default: f64| -> [f64; 2] {
        let a = doc
            .get("transform")
            .and_then(|t| t.get(k))
            .and_then(Value::as_array);
        let at = |i: usize| number(a.and_then(|a| a.get(i))).unwrap_or(default);
        [at(0), at(1)]
    };
    let (scale, translate) = (transform("scale", 1.0), transform("translate", 0.0));
    // A vertex that is not two numbers is None, and so is a ring through it.
    let vertices: Vec<Option<(f64, f64)>> = doc
        .get("vertices")
        .and_then(Value::as_array)
        .ok_or_else(|| PackError::Invalid("CityJSON without vertices".into()))?
        .iter()
        .map(|v| {
            let x = number(v.get(0))?;
            let y = number(v.get(1))?;
            Some((x * scale[0] + translate[0], y * scale[1] + translate[1]))
        })
        .collect();
    let objects = doc
        .get("CityObjects")
        .and_then(Value::as_object)
        .ok_or_else(|| PackError::Invalid("CityJSON without CityObjects".into()))?;
    let (mut out, mut skipped) = (Vec::new(), 0usize);
    for (key, obj) in objects {
        if obj.get("type").and_then(Value::as_str) != Some("Building") {
            continue;
        }
        let attrs = obj.get("attributes");
        let ground = number(attrs.and_then(|a| a.get(&heights.ground)));
        let roof = number(attrs.and_then(|a| a.get(&heights.roof)));
        let mut surfaces = lod0_surfaces(obj);
        if surfaces.is_empty() {
            for child in obj
                .get("children")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(part) = child.as_str().and_then(|c| objects.get(c)) {
                    surfaces.extend(lod0_surfaces(part));
                }
            }
        }
        let (Some(ground), Some(roof)) = (ground, roof) else {
            skipped += 1;
            continue;
        };
        let mut ring_xy = |ring: &Vec<usize>| -> Option<Vec<(f64, f64)>> {
            ring.iter()
                .map(|&i| {
                    vertices
                        .get(i)
                        .copied()
                        .flatten()
                        .and_then(|(x, y)| to_pack(x, y))
                })
                .collect()
        };
        let rings: Option<Vec<Polygon>> = surfaces
            .iter()
            .map(|surface| {
                let (exterior, holes) = surface.split_first()?;
                Some(Polygon {
                    exterior: ring_xy(exterior)?,
                    interiors: holes.iter().map(&mut ring_xy).collect::<Option<_>>()?,
                })
            })
            .collect();
        let Some(rings) = rings.filter(|r| !r.is_empty()) else {
            skipped += 1;
            continue;
        };
        let (area, e, n) = footprint(&rings);
        if area < 1.0 || roof <= ground {
            skipped += 1; // slivers, and buildings with no height
            continue;
        }
        let id = attrs
            .and_then(|a| a.get("identificatie"))
            .and_then(Value::as_str)
            .unwrap_or(key)
            .to_string();
        out.push(Lod2Building {
            id,
            e,
            n,
            ground_z: ground,
            area_m2: area,
            height_m: roof - ground,
            source: HeightSource::Lod2,
            rings,
        });
    }
    Ok((out, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_building_is_its_footprint_and_its_two_heights() {
        let doc = r#"{"type":"CityJSON","transform":{"scale":[0.001,0.001,0.001],"translate":[1000,2000,0]},
          "vertices":[[0,0,0],[10000,0,0],[10000,10000,0],[0,10000,0],[2000,2000,0],[4000,2000,0],[4000,4000,0]],
          "CityObjects":{
            "B1":{"type":"Building","attributes":{"identificatie":"NL.1","g":1.5,"r":11.5},
                  "geometry":[{"type":"MultiSurface","lod":"0","boundaries":[[[0,1,2,3]]]}],"children":["B1-0"]},
            "B1-0":{"type":"BuildingPart","geometry":[]},
            "B2":{"type":"Building","attributes":{"g":1.0},
                  "geometry":[{"type":"MultiSurface","lod":"0","boundaries":[[[4,5,6]]]}]}}}"#;
        let h = Heights {
            ground: "g".into(),
            roof: "r".into(),
        };
        let (got, skipped) = parse_cityjson(doc.as_bytes(), &h, |x, y| Some((x + 1.0, y))).unwrap();
        assert_eq!((got.len(), skipped), (1, 1), "B2 has no roof height");
        let b = &got[0];
        assert_eq!(b.id, "NL.1");
        assert!((b.area_m2 - 100.0).abs() < 1e-6, "{}", b.area_m2);
        assert!(
            (b.e - 1006.0).abs() < 1e-6 && (b.n - 2005.0).abs() < 1e-6,
            "{} {}",
            b.e,
            b.n
        );
        assert!((b.height_m - 10.0).abs() < 1e-9 && (b.ground_z - 1.5).abs() < 1e-9);
    }
}
