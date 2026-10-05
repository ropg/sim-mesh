//! `planner-web` — a local web UI for the planner.
//!
//! One Rust binary that renders pack views and serves them to the browser
//! the user already has open. This is NOT a bundled browser runtime: there
//! is no WebView, no Electron, no Chromium shipped. The page is a single
//! inlined HTML file with no framework and no network dependencies, so the
//! whole UI works with the machine offline.
//!
//! Performance/footprint rules (this may eventually run on the 2-core/2 GB
//! multi-tenant VPS):
//! - readers are opened ONCE and reused; windows are read on demand
//! - a semaphore caps concurrent renders, so load sheds instead of thrashing
//! - render size and coverage radius are hard-capped per request
//! - responses are PNG, decoded by the browser's own image path
//! - nothing is cached in RAM beyond the open COG readers' bounded caches,
//!   and the coverage rasters `/coverage/bands.bin` combines (bounded too)

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use clap::Parser;
use planner_core::geo::Xy;
use planner_pack::{LayerKind, PackManifest};
use planner_render::{BaseLayer, RenderOpts, ViewRect};
use planner_terrain::cog::CogMeta;
use planner_terrain::cog::CogReader;
#[cfg(unix)]
use planner_terrain::cog::SharedRows;
use rayon::prelude::*;
use serde::Deserialize;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

const MAX_PIXELS: u32 = 4_000_000; // ~2000×2000; bounds a single response
const MAX_COVERAGE_RADIUS_KM: f64 = 30.0;

#[derive(Parser)]
#[command(name = "planner-web", about = "Local web UI for the mesh deployment planner")]
struct Cli {
    /// Pack directory to serve.
    #[arg(long)]
    pack: PathBuf,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Concurrent slots for cheap requests (tiles, roads, areas).
    /// Concurrent slots for cheap requests (tiles, basemaps, roads, areas).
    ///
    /// Default: cores minus two, never below four. Four was the right number
    /// for the 2-core VPS and the wrong one everywhere else: a single tile
    /// update now issues a tile, a basemap and the overlay windows together,
    /// and a wheel gesture leaves the previous notch's render finishing
    /// server-side. Measured on a 12-core machine with four slots, six
    /// basemap requests in flight had two refused with 429 at once — and a
    /// refused basemap is a 1.1-1.5 s bake on the browser's main thread.
    #[arg(long)]
    render_slots: Option<usize>,
    /// Concurrent propagation SWEEPS. Bounded by memory, not cores: one
    /// 20 km sweep on a 5 m pack holds a ~400 MB radial table plus its
    /// terrain and clutter windows, so two of them is already most of a
    /// 2 GB VPS.
    #[arg(long, default_value_t = 1)]
    sweep_slots: usize,
    /// Concurrent point-to-point link requests, a `/link.json` pair or a
    /// `/links.json` batch each. Default: as the render slots, which is also
    /// how many pairs a pack's loss table asks at once. Their own, so a
    /// table's burst of pairs is never what a tile is refused for.
    #[arg(long)]
    link_slots: Option<usize>,
    /// Threads a propagation sweep runs on. Default: cores minus two, never
    /// below one. On a host shared with others, or one running several
    /// sidecars, the caller says how many are its to use.
    #[arg(long)]
    sweep_threads: Option<usize>,
    /// Build the saved index of the pack's buildings, or check the saved one
    /// is current, and exit without serving: so the first sidecar on a new
    /// pack starts with its footprints rather than parsing for them.
    #[arg(long)]
    index_buildings: bool,
    /// The coverage rasters sim-mesh's front caches (its testbed/coverage),
    /// which `/coverage/bands.bin` combines for a view, as
    /// `<dir>/<geodata>/<key>.bin`. Without it that route answers 404.
    #[arg(long)]
    coverage_dir: Option<PathBuf>,
}

/// One raster layer of the pack, readable by any number of requests at once
/// where its layout allows.
///
/// `CogReader` seeks one shared file handle, so it is `&mut` and sits behind
/// a lock, and every request that read a window waited for the one before it:
/// a tile waited out a sweep's 40 km window, and a pack table's pairs were
/// read one at a time however many render slots there were (at 8 slots, 173
/// nodes, ~15 k pairs, the reads were 58 s of a 61 s build). A layer whose
/// rows are directly readable is read by positioned reads instead
/// (`SharedRows`), which need no lock; the grid is the one `CogReader`
/// returns. `shared` is `None` where a layer's layout allows no such read,
/// and that layer is read through its lock as before.
struct Layer {
    locked: Mutex<CogReader<BufReader<File>>>,
    #[cfg(unix)]
    shared: Option<SharedRows>,
    /// The layer's grid, as the reader opened it, for working out which
    /// cells a window holds without asking the reader.
    meta: CogMeta,
}

impl Layer {
    fn new(reader: CogReader<BufReader<File>>) -> Self {
        Layer {
            #[cfg(unix)]
            shared: reader.shared_rows(),
            meta: *reader.meta(),
            locked: Mutex::new(reader),
        }
    }

    /// `CogReader::window`, read without the lock where the layer allows it.
    /// Blocking: call it from a blocking thread or under `block_in_place`.
    fn window(
        &self,
        lo: Xy,
        hi: Xy,
    ) -> Result<planner_terrain::Grid, planner_terrain::TerrainError> {
        #[cfg(unix)]
        if let Some(read) = self.shared.as_ref().and_then(|s| s.window(lo, hi).transpose()) {
            return read;
        }
        self.locked.blocking_lock().window(lo, hi)
    }

    /// `CogReader::window_max`, likewise.
    fn window_max(
        &self,
        lo: Xy,
        hi: Xy,
        max_w: usize,
        max_h: usize,
    ) -> Result<planner_terrain::Grid, planner_terrain::TerrainError> {
        #[cfg(unix)]
        if let Some(read) =
            self.shared.as_ref().and_then(|s| s.window_max(lo, hi, max_w, max_h).transpose())
        {
            return read;
        }
        self.locked.blocking_lock().window_max(lo, hi, max_w, max_h)
    }
}

struct Layers {
    terrain: Layer,
    clutter: Option<Layer>,
    population: Option<Layer>,
    classes: Option<Layer>,
    /// Fraction of each cell covered by a building footprint, 0..1.
    ///
    /// The unblended half of `clutter`. A cell at 0 is street, courtyard,
    /// park or water; at 1 it is building interior; in between it straddles a
    /// facade. Measured on the Berlin 5 m pack, that middle case is 4.3% of
    /// the region and 66% of every cell that touches a building at all — so
    /// it is the common case in the built city, not an edge.
    ///
    /// `None` on any pack built before the layer existed, which is why every
    /// consumer must keep working without it.
    built_fraction: Option<Layer>,
    /// Representative height of the buildings in a cell, m above ground.
    ///
    /// The obstacle a path crossing the building actually meets, with no
    /// built-fraction scaling applied. `clutter` mixes this with coverage and
    /// is the right input for an area sweep that treats every cell as a
    /// receiver; this is the right input for one specific path.
    building_top: Option<Layer>,
}

/// `/link.json`'s four windows over a pair's box — terrain, clutter,
/// building top, built fraction. A side layer the pack lacks, or that could
/// not read the box, is `None`.
struct LinkWindows {
    terrain: planner_terrain::Grid,
    clutter: Option<planner_terrain::Grid>,
    building_top: Option<planner_terrain::Grid>,
    built_fraction: Option<planner_terrain::Grid>,
}

impl LinkWindows {
    /// The windows over `(lo, hi)`. Blocking.
    ///
    /// Read one after another on the link's own thread. Joined in rayon's
    /// global pool they were faster for one link, but that pool is the
    /// tiles', and a pack table keeps every link slot busy: measured over a
    /// 16-way burst of links, tiles took a median 6.5 ms read in the pool and
    /// 5.2 ms read here, for 3.5% fewer links.
    fn read(
        layers: &Layers,
        (lo, hi): (Xy, Xy),
    ) -> Result<LinkWindows, planner_terrain::TerrainError> {
        let read = |l: &Option<Layer>| l.as_ref().and_then(|l| l.window(lo, hi).ok());
        Ok(LinkWindows {
            terrain: layers.terrain.window(lo, hi)?,
            clutter: read(&layers.clutter),
            building_top: read(&layers.building_top),
            built_fraction: read(&layers.built_fraction),
        })
    }

    fn views(&self) -> LinkViews<'_> {
        LinkViews {
            terrain: self.terrain.view(),
            clutter: self.clutter.as_ref().map(planner_terrain::Grid::view),
            building_top: self.building_top.as_ref().map(planner_terrain::Grid::view),
            built_fraction: self.built_fraction.as_ref().map(planner_terrain::Grid::view),
        }
    }
}

/// One layer read once for a batch of pairs, over the box that holds all of
/// theirs.
struct SharedWindow {
    grid: planner_terrain::Grid,
    meta: CogMeta,
    /// The read's first column and row in the layer.
    c0: u32,
    r0: u32,
}

impl SharedWindow {
    /// The layer's window over `(lo, hi)`. Blocking.
    fn read(layer: &Layer, (lo, hi): (Xy, Xy)) -> Result<Self, planner_terrain::TerrainError> {
        let (c0, r0, _, _) = layer.meta.window_box(lo, hi)?;
        Ok(SharedWindow { grid: layer.window(lo, hi)?, meta: layer.meta, c0, r0 })
    }

    /// What the layer's own window over `(lo, hi)` would have been: the same
    /// cells, at the origin that window would have had. `None` where this
    /// read does not hold them.
    fn view(&self, (lo, hi): (Xy, Xy)) -> Option<planner_terrain::GridView<'_>> {
        let (c0, r0, w, h) = self.meta.window_box(lo, hi).ok()?;
        let (dc, dr) = (c0.checked_sub(self.c0)?, r0.checked_sub(self.r0)?);
        self.grid.sub_view(dc as usize, dr as usize, w, h, self.meta.window_origin(c0, r0))
    }
}

/// Cells per layer a batch reads once and shares among its pairs: 16 M, a
/// 64 MB read a layer, the whole of a 20 km city at 5 m. A batch whose pairs
/// span more reads each pair's own windows, as `/link.json` does.
const BATCH_SHARED_CELLS: usize = 16 << 20;

/// `/link.json`'s four layers, each read once for a batch of pairs.
struct SharedLinkWindows {
    terrain: SharedWindow,
    clutter: Option<SharedWindow>,
    building_top: Option<SharedWindow>,
    built_fraction: Option<SharedWindow>,
}

impl SharedLinkWindows {
    /// The layers over `(lo, hi)`, when that box is within
    /// `BATCH_SHARED_CELLS` and every layer the pack has reads it; else
    /// `None`, and the pairs read their own. Blocking.
    fn read(layers: &Layers, (lo, hi): (Xy, Xy)) -> Option<Self> {
        let (_, _, w, h) = layers.terrain.meta.window_box(lo, hi).ok()?;
        if w * h > BATCH_SHARED_CELLS {
            return None;
        }
        // A side layer that cannot read the whole box may still read a pair's
        // own, which a shared `None` would hide: then nothing is shared.
        let side = |l: &Option<Layer>| match l {
            None => Ok(None),
            Some(l) => SharedWindow::read(l, (lo, hi)).map(Some),
        };
        Some(SharedLinkWindows {
            terrain: SharedWindow::read(&layers.terrain, (lo, hi)).ok()?,
            clutter: side(&layers.clutter).ok()?,
            building_top: side(&layers.building_top).ok()?,
            built_fraction: side(&layers.built_fraction).ok()?,
        })
    }

    /// A pair's views, those of its own windows over `bx`; `None` where any
    /// is not held here.
    fn views(&self, bx: (Xy, Xy)) -> Option<LinkViews<'_>> {
        fn side(
            s: &Option<SharedWindow>,
            bx: (Xy, Xy),
        ) -> Option<Option<planner_terrain::GridView<'_>>> {
            match s {
                None => Some(None),
                Some(s) => s.view(bx).map(Some),
            }
        }
        Some(LinkViews {
            terrain: self.terrain.view(bx)?,
            clutter: side(&self.clutter, bx)?,
            building_top: side(&self.building_top, bx)?,
            built_fraction: side(&self.built_fraction, bx)?,
        })
    }
}

struct AppState {
    manifest: PackManifest,
    layers: Layers,
    /// Full pack extent in pack CRS, and the WGS84 centre for the client.
    extent: ViewRect,
    centre_lat: f64,
    centre_lon: f64,
    utm: proj4rs::Proj,
    ll: proj4rs::Proj,
    /// Concurrency for CHEAP requests — tiles, roads, areas.
    ///
    /// After the direct row-read fix a full-screen tile is 11–323 ms, so these
    /// must never queue behind a propagation sweep.
    slots: Semaphore,
    /// Concurrency for point-to-point LINKS, each a profile and four P.1812
    /// runs, and for batches of them (`/links.json`), whose pairs run in
    /// `sweep_pool`.
    ///
    /// They held render slots, and a pack table asks as many pairs at once as
    /// there are slots, so while a table filled every tile and basemap the
    /// page asked was refused with 429. Links refuse each other here; a 429
    /// from `slots` again means the map itself asks more than the machine
    /// renders.
    link_slots: Semaphore,
    /// Concurrency for EXPENSIVE requests — the point-to-area sweeps.
    ///
    /// Separate from `slots` because they are now four orders of magnitude
    /// apart in cost and were sharing one pool of two. Once the azimuth count
    /// became the geometric one, a 20 km sweep went from a couple of seconds
    /// to 170–255 s while holding a slot the whole time; two of them took the
    /// pool and every tile fetch in the UI was shed with 429, so the map went
    /// blank and reported "coverage error" while the answer it was waiting for
    /// was computing correctly.
    ///
    /// Memory, not CPU, sets this: one 20 km sweep on a 5 m pack holds a
    /// ~400 MB radial table plus its terrain and clutter windows.
    sweep_slots: Semaphore,
    /// Thread pool the propagation sweeps run in — NOT the global one.
    ///
    /// Splitting the semaphores stopped tiles being refused with 429 but did
    /// not make them fast: a sweep fans 25 000 azimuth tasks into rayon's
    /// global pool, and `tile_bin`'s own `rayon::join` then queues behind all
    /// of them. Measured, a tile requested during a sweep took 16.4 s — a 200
    /// instead of a 429, which is not an improvement anyone can see.
    ///
    /// With a dedicated pool the sweep can still use most of the machine, but
    /// interactive work never waits in the same queue. Sized to leave two
    /// threads for the tiles that keep the map alive while the sweep runs.
    sweep_pool: rayon::ThreadPool,
    pack_dir: PathBuf,
    /// Road/rail geometry with precomputed bounding boxes. The boxes are
    /// computed ONCE at load: recomputing them per frame meant walking all
    /// 155 k points on every pan, which dominated render time.
    roads: Vec<(planner_pack::roads::Way, [f32; 4])>,
    /// Offline gazetteer. Empty when the pack predates the places layer —
    /// search then returns nothing rather than reaching for a geocoder.
    places: planner_pack::places::Gazetteer,
    /// The network already on the air. Display AND a planning input: "what
    /// does a site HERE add" is unanswerable without it.
    nodes: Vec<planner_pack::nodes::DeployedNode>,
    /// Per-building LoD2 heights, bucketed on a 100 m grid.
    ///
    /// The clutter raster is a cell MEAN — one representative obstruction
    /// height per 10 m cell, and outside the lidar footprint it is synthesized
    /// from a 30 m DSM. That is the right input for an area sweep, where every
    /// cell is a receiver and the mean is what a receiver in that cell sees on
    /// average. It is the WRONG input for one specific path: a link that
    /// passes over a courtyard and a link that passes through a 22 m
    /// Vorderhaus get the same averaged obstacle. Where LoD2 covers the path,
    /// the actual building height is used instead.
    ///
    /// Indexed on a BACKGROUND thread, because parsing the file is the one
    /// startup cost that scales with the pack: a full
    /// Berlin file is 1 026 069 records, and doing it before `bind` left the
    /// server unanswerable for ~30 s on a 5 m city pack. Every other layer is
    /// already lazy -- the rasters are COGs read window-by-window on demand,
    /// and roads/places/nodes are small.
    ///
    /// `RwLock` rather than `OnceLock` because readers must be able to see
    /// "not ready yet" and say so. A `/link.json` answered before the index
    /// lands falls back to the clutter raster and is a DIFFERENT number from
    /// the same request a few seconds later -- measured at 120.0 dB against
    /// 120.7 dB on a 1 km city path. The reply carries
    /// `profile_evidence.buildings_index` so that difference is visible rather
    /// than being an unexplained change in a saved result.
    buildings: Arc<std::sync::RwLock<BuildingsIndexState>>,
    /// How far the background index has got, for `/buildings/status`.
    buildings_progress: Arc<IndexProgress>,
    /// The deployed network's coverage census.
    ///
    /// This is minutes of work, not milliseconds — 269 sweeps over a city — so
    /// it cannot ride on a request the way `/loss.bin` does. It runs once in a
    /// background task and the page polls; a browser that gives up waiting
    /// does not cancel it, and a second viewer joins the same result instead
    /// of starting another.
    network: Mutex<NetworkCensus>,
    /// Last computed coverage, keyed by the parameters that produced it.
    ///
    /// The P.1812 sweep depends ONLY on the transmitter and link settings —
    /// not on where the map is panned or how far it is zoomed. Caching it
    /// means heavy work happens exactly when the user changes a transmitter
    /// parameter, and never on navigation.
    coverage_cache: Mutex<Option<CachedCoverage>>,
    /// `--coverage-dir`.
    coverage_dir: Option<PathBuf>,
    /// The coverage rasters `/coverage/bands.bin` has read, each with the
    /// terrain under it, the most recently used last, up to
    /// `COVERAGE_KEPT_BYTES`: the front's, by file, and the bands of a sweep
    /// still growing, by file and band.
    coverage_rasters: std::sync::Mutex<Vec<(String, Arc<CoverageRaster>)>>,
    /// A sweep growing outward from the transmitter.
    ///
    /// The single-shot `/loss.bin` was fine when a sweep took seconds. At the
    /// geometric azimuth count a 20 km sweep is 170–255 s, and a request that
    /// answers nothing for four minutes is indistinguishable from a broken
    /// button — the operator's words were "placing tx node does nothing".
    ///
    /// The fix is not to compute less. Cost goes as r², so a band out to 1 km
    /// is 1/400th of the 20 km job: the map can show a finished, CORRECT
    /// answer near the transmitter almost immediately and grow it outward.
    /// Each band is a complete sweep at its own radius with the full
    /// geometric azimuth count for that radius, not a coarse preview that
    /// gets refined — so nothing on screen is ever a number that will later
    /// change. Because each band quadruples, the whole ladder costs about
    /// 1.33× the single-shot sweep, and it also removes the oversampling the
    /// old fixed count caused: 25 133 radials is right at 20 km and absurd at
    /// 500 m.
    sweep: Mutex<ProgressiveSweep>,
}

/// Radii a progressive sweep publishes at, as a fraction of the requested
/// radius. Doubling, so each band is ~4× the previous and the early ones are
/// effectively free.
const SWEEP_BANDS: &[f64] = &[0.05, 0.1, 0.2, 0.4, 0.7, 1.0];

#[derive(Default)]
enum ProgressiveSweep {
    #[default]
    Idle,
    Running {
        key: CoverageKey,
        /// Bands in this sweep: the ladder's, or one for a `whole` sweep.
        bands: usize,
        /// Bands finished so far, and the radius of the last one.
        done: usize,
        band_radius_m: f64,
        started: Option<std::time::Instant>,
        /// Set by a superseding request to stop this sweep. Held here rather
        /// than in the task so a request that arrives while the previous
        /// sweep is mid-band can reach it — waiting for the band to end would
        /// be up to 400 s on a 20 km ladder.
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
    Failed(String),
}

#[derive(PartialEq, Clone, Copy)]
struct CoverageKey {
    lat: f64,
    lon: f64,
    tx_h: f64,
    rx_h: f64,
    radius_km: f64,
    budget_db: f32,
    azimuths: usize,
}

/// One LoD2 building, reduced to what a path profile needs.
#[derive(Clone, Copy)]
struct Bldg {
    /// Centroid, as an offset from [`BuildingIndex::origin`] — NOT an
    /// absolute coordinate.
    ///
    /// An f32 holding a UTM northing has a step of exactly 0.5 m at Berlin's
    /// 5.82e6, so storing the coordinate directly quantized every building's
    /// position to half a metre on a 5 m pack whose whole purpose is now
    /// facade boundaries. Referred to the pack origin the span is at most
    /// ~43 km and the step is 0.004 m.
    x: f32,
    y: f32,
    /// Radius of a disc with the footprint's area.
    ///
    /// The containment test for a pack built before `--lod2-geometry`, which
    /// stores a centroid and an area and nothing else. Coarse for a long thin
    /// Berlin block wing, but bounded and honest, and far closer than a cell
    /// mean that already averaged the building with the courtyard beside it.
    ///
    /// It is NOT a bounding radius and must never be used to reject a
    /// candidate. An equal-area disc has the footprint's area but not its
    /// shape, so part of every real building lies outside it: measured over
    /// 40 000 Berlin footprints, 100% extend past it, by a median factor of
    /// 3.38 and a p90 of 33.7. Using it as a broad phase silently discarded
    /// most of each footprint before the exact polygon test could run.
    r: f32,
    /// True bounding radius: the farthest vertex from the centroid.
    ///
    /// Equals `r` when the pack carries no polygons, which is correct — with
    /// nothing but a disc, the disc IS the building.
    br: f32,
    /// Absolute height of the roof (ground_z + height_m), metres.
    top_masl: f32,
    /// Range into [`BuildingIndex::polys`]. Empty when the pack carries no
    /// footprint geometry.
    poly_start: u32,
    poly_end: u32,
}

/// Buildings bucketed on a fixed grid so a path query touches a handful of
/// cells instead of every record. A full-Berlin LoD2 pack is hundreds of
/// thousands of buildings; a linear scan per profile sample would be
/// milliseconds per point.
#[derive(Default)]
struct BuildingIndex {
    /// Buildings bucketed by every grid cell their BOUNDING BOX overlaps, so
    /// a query reads one cell and is done.
    ///
    /// The previous scheme stored each building in the single cell holding
    /// its centroid and widened the query by the largest radius in the pack.
    /// That makes one malformed footprint expensive for every lookup —
    /// the widest here reaches 1957x its equal-area radius — and it made the
    /// reach a property of the worst record rather than of the query.
    cells: std::collections::HashMap<(i32, i32), Vec<u32>>,
    /// The buildings themselves, referenced by index from `cells`.
    items: Vec<Bldg>,
    count: usize,
    /// Frame for every stored coordinate. See [`Bldg::x`] for why.
    origin: (f64, f64),
    /// Footprint geometry, flattened into three arenas.
    ///
    /// A `Vec<Vec<Vec<(f32,f32)>>>` per building would be three allocations
    /// each over a million buildings; these are three allocations total.
    /// Vertices are offsets from their own building's centroid, so f32 is
    /// good to ~3e-5 m across any real footprint.
    ///
    /// `polys[i]` is the exclusive end index into `rings` for polygon `i`;
    /// `rings[j]` the exclusive end into `verts` for ring `j`. Ring 0 of a
    /// polygon is its outline and the rest are courtyards, which is what
    /// makes even-odd within a polygon the correct containment test.
    polys: Vec<u32>,
    rings: Vec<u32>,
    verts: Vec<[f32; 2]>,
    /// Buildings whose footprint polygons are known, as opposed to
    /// approximated by an equal-area disc. Reported so an answer can say
    /// which it rested on rather than presenting the two alike.
    with_geometry: usize,
    /// Each building's footprint area (m²) and height above its ground (m),
    /// as its record gave them, by the same index as `items`: what planner's
    /// height estimator takes of a building (`/height.json`), kept beside the
    /// records a path reads rather than in them.
    area_height: Vec<[f32; 2]>,
    /// The building file this index was built from, where its mtime could be
    /// read: what a `/buildings.bin` validator rests on.
    stamp: Option<FileStamp>,
}

const BLDG_CELL_M: f32 = 100.0;

/// Where the background building index has got to.
///
/// Three states and not two, because "still loading" and "this pack has no
/// buildings" produce the SAME link answer (the clutter-raster fallback) and
/// must not produce the same explanation. One is temporary and the number will
/// change; the other is permanent and it will not.
enum BuildingsIndexState {
    /// A background thread is parsing. Links use the clutter raster meanwhile.
    Loading,
    /// Shared, so a request can take the index and let the lock go.
    Ready(Arc<BuildingIndex>),
    /// No Buildings layer in the manifest, or the file could not be read.
    Absent,
}

impl BuildingsIndexState {
    /// The index if it is usable, else `None` — callers fall back to clutter.
    fn get(&self) -> Option<&BuildingIndex> {
        match self {
            BuildingsIndexState::Ready(b) => Some(b),
            _ => None,
        }
    }

    /// The index to keep past the lock, if it is usable.
    fn ready(&self) -> Option<Arc<BuildingIndex>> {
        match self {
            BuildingsIndexState::Ready(b) => Some(Arc::clone(b)),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            BuildingsIndexState::Loading => "loading",
            BuildingsIndexState::Ready(_) => "ready",
            BuildingsIndexState::Absent => "absent",
        }
    }
}

/// One line of `buildings.jsonl`.
///
/// A typed struct rather than `serde_json::Value`: the Value path allocated a
/// map and six owned values per record and then threw them away, which over a
/// million records is most of the parse cost.
#[derive(serde::Deserialize)]
struct BuildingRecord {
    e: f64,
    n: f64,
    area_m2: f64,
    height_m: f64,
    #[serde(default)]
    ground_z: f64,
    /// Footprint polygons, present only when the pack was built with
    /// `--lod2-geometry`. Absent on every older pack, which is why the index
    /// keeps the equal-area disc as a fallback rather than requiring this.
    #[serde(default)]
    rings: Vec<BuildingPolygon>,
}

#[derive(serde::Deserialize)]
struct BuildingPolygon {
    exterior: Vec<(f64, f64)>,
    #[serde(default)]
    interiors: Vec<Vec<(f64, f64)>>,
}

impl BuildingIndex {
    /// Buildings wider than this are treated as corrupt geometry and indexed
    /// by their equal-area disc instead. Nothing in Berlin is 1 km across;
    /// a footprint that claims to be has a vertex in the wrong place, and
    /// bucketing it honestly would put it in 400 cells.
    const MAX_PLAUSIBLE_SPAN_M: f32 = 1000.0;

    fn insert(&mut self, b: Bldg) -> bool {
        let idx = self.items.len() as u32;
        let suspect = b.br > Self::MAX_PLAUSIBLE_SPAN_M;
        let reach = if suspect { b.r } else { b.br };
        let (c0, c1) = (
            ((b.x - reach) / BLDG_CELL_M).floor() as i32,
            ((b.x + reach) / BLDG_CELL_M).floor() as i32,
        );
        let (r0, r1) = (
            ((b.y - reach) / BLDG_CELL_M).floor() as i32,
            ((b.y + reach) / BLDG_CELL_M).floor() as i32,
        );
        self.items.push(b);
        for cx in c0..=c1 {
            for cy in r0..=r1 {
                self.cells.entry((cx, cy)).or_default().push(idx);
            }
        }
        self.count += 1;
        suspect
    }

    /// Is `p` (offset from the building's centroid) inside this building?
    ///
    /// Even-odd per POLYGON — outline plus its own courtyards — so a point in
    /// a Blockrand Innenhof crosses two boundaries and is correctly OUTSIDE.
    /// Deliberately not even-odd across the whole building: LoD2 splits a
    /// block into parts whose footprints touch at shared walls, and a
    /// building-wide pass would cancel two wings against each other and
    /// report the wall between them as open sky.
    fn contains(&self, b: &Bldg, px: f32, py: f32) -> bool {
        for pi in b.poly_start..b.poly_end {
            let r0 = if pi == 0 { 0 } else { self.polys[pi as usize - 1] };
            let r1 = self.polys[pi as usize];
            let mut inside = false;
            for ri in r0..r1 {
                let v0 = if ri == 0 { 0 } else { self.rings[ri as usize - 1] } as usize;
                let v1 = self.rings[ri as usize] as usize;
                let ring = &self.verts[v0..v1];
                let n = ring.len();
                if n < 3 {
                    continue;
                }
                let mut j = n - 1;
                for i in 0..n {
                    let (xi, yi) = (ring[i][0], ring[i][1]);
                    let (xj, yj) = (ring[j][0], ring[j][1]);
                    if (yi > py) != (yj > py)
                        && px < (xj - xi) * (py - yi) / (yj - yi) + xi
                    {
                        inside = !inside;
                    }
                    j = i;
                }
            }
            if inside {
                return true;
            }
        }
        false
    }

    /// Footprint outlines overlapping a world-space bbox, as flat vertex
    /// runs in the index's own frame.
    ///
    /// Exists because the terminal correction reads the building the node is
    /// standing ON, so a transmitter dropped ten metres off lands on a
    /// different roof — or in the street — and the whole coverage pattern
    /// changes. Placement was a click on a hillshade; it now has to be a
    /// click on a specific building, and that is only possible if the
    /// buildings are drawn.
    ///
    /// Returns `(vertices, ring_lengths, tops, truncated)`. Courtyards are
    /// included as their own rings: an Innenhof is open sky and drawing a
    /// block as a solid slab would misplace exactly the kind of node this is
    /// meant to help site.
    /// Footprint rings inside a box: `(verts, ring_lens, ring_top_masl,
    /// ring_building_id, truncated)`.
    ///
    /// The id is the building's index in `items` -- stable for the life of the
    /// index, i.e. for the life of the process on one pack. It is what the
    /// page's footprint cache keys on, so a building fetched once keeps its
    /// geometry across every later pan and zoom, and a later fetch of the same
    /// box is one f32 per building rather than its outline again.
    fn outlines_in(
        &self,
        min: Xy,
        max: Xy,
        max_vertices: usize,
    ) -> (Vec<[f32; 2]>, Vec<u32>, Vec<f32>, Vec<u32>, bool) {
        let (x0, y0) = ((min.x - self.origin.0) as f32, (min.y - self.origin.1) as f32);
        let (x1, y1) = ((max.x - self.origin.0) as f32, (max.y - self.origin.1) as f32);
        let (c0, c1) = ((x0 / BLDG_CELL_M).floor() as i32, (x1 / BLDG_CELL_M).floor() as i32);
        let (r0, r1) = ((y0 / BLDG_CELL_M).floor() as i32, (y1 / BLDG_CELL_M).floor() as i32);
        let mut seen = std::collections::HashSet::new();
        let (mut verts, mut lens, mut tops) = (Vec::new(), Vec::new(), Vec::new());
        // The building's id, repeated onto each of its rings, so the page can
        // group the rings of one building and give the whole of it ONE value.
        let mut ids: Vec<u32> = Vec::new();
        let mut truncated = false;
        'cells: for cx in c0..=c1 {
            for cy in r0..=r1 {
                let Some(list) = self.cells.get(&(cx, cy)) else { continue };
                for &i in list {
                    // A building spans several cells, so the same index
                    // arrives more than once.
                    if !seen.insert(i) {
                        continue;
                    }
                    let b = &self.items[i as usize];
                    if b.x + b.br < x0 || b.x - b.br > x1 || b.y + b.br < y0 || b.y - b.br > y1 {
                        continue;
                    }
                    if verts.len() >= max_vertices {
                        truncated = true;
                        break 'cells;
                    }
                    for pi in b.poly_start..b.poly_end {
                        let rs = if pi == 0 { 0 } else { self.polys[pi as usize - 1] };
                        let re = self.polys[pi as usize];
                        for ri in rs..re {
                            let vs = if ri == 0 { 0 } else { self.rings[ri as usize - 1] } as usize;
                            let ve = self.rings[ri as usize] as usize;
                            if ve - vs < 3 {
                                continue;
                            }
                            for v in &self.verts[vs..ve] {
                                verts.push([b.x + v[0], b.y + v[1]]);
                            }
                            lens.push((ve - vs) as u32);
                            tops.push(b.top_masl);
                            ids.push(i);
                        }
                    }
                }
            }
        }
        (verts, lens, tops, ids, truncated)
    }

    /// Every building id whose bounding box meets `min..max`. The geometry-free
    /// twin of `outlines_in`, for refreshing values on footprints the page
    /// already holds.
    fn ids_in(&self, min: Xy, max: Xy) -> Vec<u32> {
        let (x0, y0) = ((min.x - self.origin.0) as f32, (min.y - self.origin.1) as f32);
        let (x1, y1) = ((max.x - self.origin.0) as f32, (max.y - self.origin.1) as f32);
        let (c0, c1) = ((x0 / BLDG_CELL_M).floor() as i32, (x1 / BLDG_CELL_M).floor() as i32);
        let (r0, r1) = ((y0 / BLDG_CELL_M).floor() as i32, (y1 / BLDG_CELL_M).floor() as i32);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for cx in c0..=c1 {
            for cy in r0..=r1 {
                let Some(list) = self.cells.get(&(cx, cy)) else { continue };
                for &i in list {
                    if !seen.insert(i) {
                        continue;
                    }
                    let b = &self.items[i as usize];
                    if b.x + b.br < x0 || b.x - b.br > x1 || b.y + b.br < y0 || b.y - b.br > y1 {
                        continue;
                    }
                    out.push(i);
                }
            }
        }
        out
    }

    /// World position of a building's centroid.
    fn centroid(&self, id: u32) -> (f64, f64) {
        let b = &self.items[id as usize];
        (self.origin.0 + b.x as f64, self.origin.1 + b.y as f64)
    }

    /// What P.2108 §3.1 needs at a terminal, measured ALONG A BEARING.
    ///
    /// Returns `(R, w_s)`: the representative clutter height that shadows this
    /// terminal in that direction, and how far away it is.
    ///
    /// This is the part of the model that no raster can supply and the part
    /// the Recommendation leaves to a nominal constant. P.2108 offers 27 m as
    /// a typical street width, and with a constant there the correction is
    /// identical in every direction from a site — a pure offset, which cannot
    /// reproduce the shadow a node below its own roofline actually casts and
    /// cannot improve a ranking that is already uninformative. Measured from
    /// the footprints, w_s is small where a facade stands close and large
    /// where the building ends, so the correction becomes directional.
    ///
    /// Walks the ray in `step` increments to `max_m` and takes the FIRST
    /// building whose roof stands above the antenna: that is the edge that
    /// diffracts. Buildings beyond it are P.1812's business — this term is
    /// only about the terminal's immediate surroundings, and charging for a
    /// second rooftop here would double-count what the path profile already
    /// carries.
    fn clutter_along(
        &self,
        x: f64,
        y: f64,
        ground_masl: f64,
        antenna_masl: f64,
        bearing_rad: f64,
        max_m: f64,
        step: f64,
    ) -> Option<(f64, f64)> {
        let (sx, sy) = (bearing_rad.sin(), bearing_rad.cos());
        let over = |t: f64| {
            self.top_at(x + sx * t, y + sy * t)
                .map(|top| top as f64)
                .filter(|&top| top > antenna_masl)
        };

        // CASE 1: the terminal is standing under a roof of its own.
        //
        // A node on a balcony is INSIDE its building's footprint, so a walk
        // that simply reports the first tall thing it meets reports that
        // building at the first step, in every direction, and the correction
        // becomes a constant — which is the whole failure this measurement
        // exists to avoid. What actually diffracts is the edge where that
        // roof ENDS, so walk to the exit and use that distance.
        //
        // The asymmetry falls straight out: a balcony on the south face is a
        // metre from the south eave and fifteen metres from the north one, so
        // the correction is small looking south and large looking north. That
        // is the shadow an operator sees and a nominal street width cannot
        // express.
        if let Some(own) = over(0.0) {
            let mut t = step;
            while t <= max_m {
                match over(t) {
                    // Still under the same roof (or a taller neighbour that
                    // has taken over the shadowing).
                    Some(_) => t += step,
                    None => return Some(((own - ground_masl).max(0.0), t.max(step))),
                }
            }
            // Under continuous building for the whole probe: deeply shadowed,
            // and the edge is at least max_m away.
            return Some(((own - ground_masl).max(0.0), max_m));
        }

        // CASE 2: the terminal is in the open — a street, a yard, a park.
        // The clutter is the first thing tall enough to shadow it.
        let mut t = step;
        while t <= max_m {
            if let Some(top) = over(t) {
                // R is above the TERMINAL's ground, which is what P.2108
                // measures the antenna height against.
                return Some(((top - ground_masl).max(0.0), t));
            }
            t += step;
        }
        None
    }

    /// Roof height of the tallest building covering this point, if any.
    ///
    /// Exact against the footprint where the pack carries one; an equal-area
    /// disc otherwise. The disc is a broad phase either way, so a polygon
    /// test only runs for buildings whose circle already contains the point.
    fn top_at(&self, x: f64, y: f64) -> Option<f32> {
        if self.cells.is_empty() {
            return None;
        }
        let xf = (x - self.origin.0) as f32;
        let yf = (y - self.origin.1) as f32;
        let key = (
            (xf / BLDG_CELL_M).floor() as i32,
            (yf / BLDG_CELL_M).floor() as i32,
        );
        let Some(v) = self.cells.get(&key) else { return None };
        let mut best: Option<f32> = None;
        for &i in v {
            let b = &self.items[i as usize];
            let (dx, dy) = (b.x - xf, b.y - yf);
            // Broad phase on the BOUNDING radius, which by construction
            // cannot reject a point the polygon contains.
            if dx * dx + dy * dy > b.br * b.br {
                continue;
            }
            let hit = if b.poly_end > b.poly_start {
                self.contains(b, xf - b.x, yf - b.y)
            } else {
                // No polygon: the equal-area disc IS the building.
                dx * dx + dy * dy <= b.r * b.r
            };
            if hit {
                best = Some(best.map_or(b.top_masl, |t: f32| t.max(b.top_masl)));
            }
        }
        best
    }

    /// Whether building `i`'s footprint holds this point.
    fn holds(&self, i: u32, x: f64, y: f64) -> bool {
        let Some(b) = self.items.get(i as usize) else { return false };
        let (xf, yf) = ((x - self.origin.0) as f32, (y - self.origin.1) as f32);
        let (dx, dy) = (b.x - xf, b.y - yf);
        if dx * dx + dy * dy > b.br * b.br {
            return false;
        }
        if b.poly_end > b.poly_start {
            self.contains(b, xf - b.x, yf - b.y)
        } else {
            dx * dx + dy * dy <= b.r * b.r
        }
    }

    /// The buildings whose centroid lies within `radius_m` of (x, y), as
    /// planner's height estimator takes them: centroid, footprint area and
    /// height above ground. The centroid is the index's own, good to a few
    /// millimetres (see [`Bldg::x`]). A building indexed without an area and
    /// height is left out.
    fn hints_near(&self, x: f64, y: f64, radius_m: f64) -> Vec<planner_coverage::environment::BuildingHint> {
        let (xf, yf) = ((x - self.origin.0) as f32, (y - self.origin.1) as f32);
        let r = radius_m as f32;
        let cell = |v: f32| (v / BLDG_CELL_M).floor() as i32;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for cx in cell(xf - r)..=cell(xf + r) {
            for cy in cell(yf - r)..=cell(yf + r) {
                for &i in self.cells.get(&(cx, cy)).map(Vec::as_slice).unwrap_or(&[]) {
                    if !seen.insert(i) {
                        continue;
                    }
                    let b = &self.items[i as usize];
                    let (dx, dy) = ((b.x - xf) as f64, (b.y - yf) as f64);
                    if dx.hypot(dy) > radius_m {
                        continue;
                    }
                    let Some(&[area_m2, height_m]) = self.area_height.get(i as usize) else {
                        continue;
                    };
                    out.push(planner_coverage::environment::BuildingHint {
                        xy: Xy { x: self.origin.0 + b.x as f64, y: self.origin.1 + b.y as f64 },
                        footprint_m2: area_m2,
                        height_m,
                    });
                }
            }
        }
        out
    }

    /// The building an antenna at (x, y), `antenna_masl` above sea level, is
    /// INSIDE: the tallest whose footprint holds the point and whose roof is
    /// above the antenna. `None` for an antenna in the open or on a roof.
    fn indoor_at(&self, x: f64, y: f64, antenna_masl: f64) -> Option<u32> {
        let (xf, yf) = ((x - self.origin.0) as f32, (y - self.origin.1) as f32);
        let key = ((xf / BLDG_CELL_M).floor() as i32, (yf / BLDG_CELL_M).floor() as i32);
        let v = self.cells.get(&key)?;
        let mut best: Option<(u32, f32)> = None;
        for &i in v {
            let b = &self.items[i as usize];
            if (b.top_masl as f64) <= antenna_masl || !self.holds(i, x, y) {
                continue;
            }
            if best.map_or(true, |(_, t)| b.top_masl > t) {
                best = Some((i, b.top_masl));
            }
        }
        best.map(|(i, _)| i)
    }
}

/// Where the index of a pack's building file is saved: beside the file.
///
/// Parsing `buildings.jsonl` is the one startup cost that grows with the
/// pack (116 MB, 270 k footprints on the Berlin centre pack), and until it
/// is done `/buildings.bin` answers 204 and links fall back to the clutter
/// raster. So the index is saved once built, and a later start loads it,
/// which is a read and a copy.
///
/// A dot file, `.<stem>.idx`: a pack's export leaves dot files out, so the
/// cache never travels without the file it was built from.
fn saved_index_path(jsonl: &Path) -> PathBuf {
    let stem = jsonl.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    jsonl.with_file_name(format!(".{stem}.idx"))
}

/// The saved index's layout, little-endian:
///
/// "PBIX" | u32 version | u64 file bytes | u64 file mtime s | u32 file mtime
/// ns | f64 origin_x | f64 origin_y | u32 buildings | u32 polygons | u32
/// rings | u32 vertices | per building: f32 x, f32 y, f32 r, f32 br, f32
/// top_masl, u32 poly_start, u32 poly_end | per building: f32 area_m2, f32
/// height_m | u32 polys[] | u32 rings[] | per vertex: f32 dx, f32 dy.
///
/// The records are the parsed ones, bit for bit, and the cells are rebuilt
/// from them by `BuildingIndex::insert` in the same order, so a loaded index
/// answers as the parsed one does. Bump the version with any change to how a
/// line becomes a `Bldg`: an index saved by another version is parsed again.
const INDEX_MAGIC: &[u8; 4] = b"PBIX";
const INDEX_VERSION: u32 = 1;
const INDEX_HEADER: usize = 60;

/// The building file an index was built from, as its size and mtime. A saved
/// index is used only while the file still has both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileStamp {
    bytes: u64,
    mtime_s: u64,
    mtime_ns: u32,
}

impl FileStamp {
    fn of(meta: &std::fs::Metadata) -> std::io::Result<Self> {
        let t = meta.modified()?.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Ok(FileStamp { bytes: meta.len(), mtime_s: t.as_secs(), mtime_ns: t.subsec_nanos() })
    }
}

impl BuildingIndex {
    /// The saved form of this index (see `INDEX_MAGIC`) for a building file
    /// stamped `stamp`.
    fn to_bytes(&self, stamp: FileStamp) -> Vec<u8> {
        let n = self.items.len();
        debug_assert_eq!(self.area_height.len(), n, "one area and height per building");
        let mut out = Vec::with_capacity(
            INDEX_HEADER
                + n * 36
                + (self.polys.len() + self.rings.len()) * 4
                + self.verts.len() * 8,
        );
        out.extend_from_slice(INDEX_MAGIC);
        out.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        out.extend_from_slice(&stamp.bytes.to_le_bytes());
        out.extend_from_slice(&stamp.mtime_s.to_le_bytes());
        out.extend_from_slice(&stamp.mtime_ns.to_le_bytes());
        out.extend_from_slice(&self.origin.0.to_le_bytes());
        out.extend_from_slice(&self.origin.1.to_le_bytes());
        for len in [n, self.polys.len(), self.rings.len(), self.verts.len()] {
            out.extend_from_slice(&(len as u32).to_le_bytes());
        }
        for b in &self.items {
            for v in [b.x, b.y, b.r, b.br, b.top_masl] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&b.poly_start.to_le_bytes());
            out.extend_from_slice(&b.poly_end.to_le_bytes());
        }
        for v in self.area_height.iter().flatten() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for i in self.polys.iter().chain(&self.rings) {
            out.extend_from_slice(&i.to_le_bytes());
        }
        for v in self.verts.iter().flatten() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// An index from its saved form, with how many of its footprints are too
    /// wide to trust, if it was saved by this version, for the building file
    /// as `stamp` has it now, on this pack's frame `origin`. `Err` says why
    /// not.
    fn from_bytes(
        bytes: &[u8],
        stamp: FileStamp,
        origin: (f64, f64),
    ) -> Result<(BuildingIndex, usize), String> {
        let head = bytes.get(..INDEX_HEADER).ok_or("it is shorter than its header")?;
        let u32_at = |o: usize| u32::from_le_bytes(head[o..o + 4].try_into().expect("4 bytes"));
        let u64_at = |o: usize| u64::from_le_bytes(head[o..o + 8].try_into().expect("8 bytes"));
        let f64_at = |o: usize| f64::from_le_bytes(head[o..o + 8].try_into().expect("8 bytes"));
        if &head[..4] != INDEX_MAGIC {
            return Err("it is not a saved building index".into());
        }
        if u32_at(4) != INDEX_VERSION {
            return Err(format!("it was saved as version {}, not {INDEX_VERSION}", u32_at(4)));
        }
        if (FileStamp { bytes: u64_at(8), mtime_s: u64_at(16), mtime_ns: u32_at(24) }) != stamp {
            return Err("the building file has changed since it was saved".into());
        }
        let same = |a: f64, b: f64| a.to_bits() == b.to_bits();
        if !(same(f64_at(28), origin.0) && same(f64_at(36), origin.1)) {
            return Err("it was saved on another grid".into());
        }
        let (n, np, nr, nv) =
            (u32_at(44) as usize, u32_at(48) as usize, u32_at(52) as usize, u32_at(56) as usize);
        let want = INDEX_HEADER + n * 36 + (np + nr) * 4 + nv * 8;
        if bytes.len() != want {
            return Err(format!("it is {} bytes where its header says {want}", bytes.len()));
        }
        let (items, rest) = bytes[INDEX_HEADER..].split_at(n * 28);
        let (area_height, rest) = rest.split_at(n * 8);
        let (polys, rest) = rest.split_at(np * 4);
        let (rings, verts) = rest.split_at(nr * 4);
        let f = |c: &[u8], o: usize| f32::from_le_bytes(c[o..o + 4].try_into().expect("4 bytes"));
        let u = |c: &[u8], o: usize| u32::from_le_bytes(c[o..o + 4].try_into().expect("4 bytes"));
        let pairs = |b: &[u8]| -> Vec<[f32; 2]> {
            b.as_chunks::<8>().0.iter().map(|c| [f(c, 0), f(c, 4)]).collect()
        };
        let words =
            |b: &[u8]| -> Vec<u32> { b.as_chunks::<4>().0.iter().map(|c| u(c, 0)).collect() };
        let mut ix = BuildingIndex {
            origin,
            area_height: pairs(area_height),
            polys: words(polys),
            rings: words(rings),
            verts: pairs(verts),
            ..Default::default()
        };
        // Every range must lie inside the arrays it indexes: a footprint
        // query slices by them, and a bad one would panic a request.
        let ascending = |v: &[u32], end: usize| {
            v.windows(2).all(|w| w[0] <= w[1]) && v.last().is_none_or(|&l| l as usize <= end)
        };
        if !ascending(&ix.polys, nr) || !ascending(&ix.rings, nv) {
            return Err("its polygon or ring ranges run outside it".into());
        }
        ix.items.reserve(n);
        let mut suspect = 0usize;
        for c in items.as_chunks::<28>().0 {
            let b = Bldg {
                x: f(c, 0),
                y: f(c, 4),
                r: f(c, 8),
                br: f(c, 12),
                top_masl: f(c, 16),
                poly_start: u(c, 20),
                poly_end: u(c, 24),
            };
            if b.poly_start > b.poly_end || b.poly_end as usize > np {
                return Err("a building's polygons run outside it".into());
            }
            if b.poly_end > b.poly_start {
                ix.with_geometry += 1;
            }
            suspect += usize::from(ix.insert(b));
        }
        Ok((ix, suspect))
    }
}

/// Write `ix` to `path` by way of a temporary file and a rename, so a reader,
/// or a second sidecar on the same pack, sees a whole index or none.
fn save_index(path: &Path, ix: &BuildingIndex, stamp: FileStamp) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("idx.{}.tmp", std::process::id()));
    let saved = std::fs::write(&tmp, ix.to_bytes(stamp)).and_then(|()| std::fs::rename(&tmp, path));
    if saved.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    saved
}

/// What the background index is doing.
#[derive(Clone, Copy)]
enum IndexPhase {
    Waiting,
    Reading,
    Parsing,
    Saving,
    Done,
}

/// How far the building index has got, for `/buildings/status`: what the
/// thread is doing and, while it parses the building file, how much of it is
/// read. A page waiting on footprints can then say how long, rather than
/// only "not yet".
struct IndexProgress {
    started: std::time::Instant,
    phase: AtomicU8,
    /// Once done: whether the index came from the saved file.
    from_saved: AtomicBool,
    bytes_read: AtomicU64,
    bytes_total: AtomicU64,
    buildings: AtomicU64,
    /// Once done: how long it took.
    took_ms: AtomicU64,
}

impl IndexProgress {
    fn new() -> Self {
        IndexProgress {
            started: std::time::Instant::now(),
            phase: AtomicU8::new(IndexPhase::Waiting as u8),
            from_saved: AtomicBool::new(false),
            bytes_read: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            buildings: AtomicU64::new(0),
            took_ms: AtomicU64::new(0),
        }
    }

    fn phase(&self, p: IndexPhase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }

    fn parsed(&self, bytes_read: u64, buildings: usize) {
        self.bytes_read.store(bytes_read, Ordering::Relaxed);
        self.buildings.store(buildings as u64, Ordering::Relaxed);
    }

    fn done(&self, from_saved: bool, buildings: usize) {
        self.from_saved.store(from_saved, Ordering::Relaxed);
        self.buildings.store(buildings as u64, Ordering::Relaxed);
        self.took_ms.store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.phase(IndexPhase::Done);
    }

    /// `/buildings/status`'s reply, `state` being the index's own.
    fn json(&self, state: &str) -> serde_json::Value {
        let phase = self.phase.load(Ordering::Relaxed);
        let done = phase == IndexPhase::Done as u8;
        let elapsed_ms = if done {
            self.took_ms.load(Ordering::Relaxed)
        } else {
            self.started.elapsed().as_millis() as u64
        };
        serde_json::json!({
            "state": state,
            "phase": match phase {
                p if p == IndexPhase::Reading as u8 => "reading the saved index",
                p if p == IndexPhase::Parsing as u8 => "parsing the building file",
                p if p == IndexPhase::Saving as u8 => "saving the index",
                p if p == IndexPhase::Done as u8 => "done",
                _ => "waiting",
            },
            "source": done.then(|| {
                if self.from_saved.load(Ordering::Relaxed) { "saved index" } else { "building file" }
            }),
            "bytes_read": self.bytes_read.load(Ordering::Relaxed),
            "bytes_total": self.bytes_total.load(Ordering::Relaxed),
            "buildings": self.buildings.load(Ordering::Relaxed),
            "elapsed_s": elapsed_ms as f64 / 1000.0,
        })
    }
}

/// Index a building file line by line, telling `progress` how far it has
/// read. Returns the index, how many lines could not be read, and how many
/// footprints are too wide to trust.
fn parse_buildings(
    f: File,
    origin: (f64, f64),
    progress: &IndexProgress,
) -> (BuildingIndex, usize, usize) {
    use std::io::BufRead;
    let mut idx = BuildingIndex { origin, ..Default::default() };
    let mut bad = 0usize;
    // Footprints too wide to be real. Counted rather than dropped: the
    // record is still a building, it just cannot be trusted to bound itself.
    let mut suspect = 0usize;
    let mut read = 0u64;
    for (k, line) in BufReader::new(f).lines().map_while(Result::ok).enumerate() {
        read += line.len() as u64 + 1;
        if k % 4096 == 0 {
            progress.parsed(read, idx.count);
        }
        let Ok(r) = serde_json::from_str::<BuildingRecord>(&line) else {
            bad += 1;
            continue;
        };
        if !(r.e.is_finite() && r.n.is_finite() && r.area_m2 > 0.0 && r.height_m.is_finite()) {
            bad += 1;
            continue;
        }
        // Vertices are stored as offsets from this building's own
        // centroid, so f32 is exact to ~3e-5 m across any footprint;
        // storing them absolutely would quantize northings to 0.5 m.
        let poly_start = idx.polys.len() as u32;
        let mut had_geometry = false;
        // Farthest vertex from the centroid, i.e. the real bounding
        // radius. Accumulated here rather than derived from the area,
        // because an equal-area disc is not a bound.
        let mut far2: f64 = 0.0;
        for poly in &r.rings {
            if poly.exterior.len() < 3 {
                continue;
            }
            for ring in std::iter::once(&poly.exterior).chain(poly.interiors.iter()) {
                if ring.len() < 3 {
                    continue;
                }
                for &(vx, vy) in ring {
                    let (ox, oy) = (vx - r.e, vy - r.n);
                    far2 = far2.max(ox * ox + oy * oy);
                    idx.verts.push([ox as f32, oy as f32]);
                }
                idx.rings.push(idx.verts.len() as u32);
            }
            idx.polys.push(idx.rings.len() as u32);
            had_geometry = true;
        }
        if had_geometry {
            idx.with_geometry += 1;
        }
        let poly_end = idx.polys.len() as u32;
        let r_eq = (r.area_m2 / std::f64::consts::PI).sqrt() as f32;
        let br = if had_geometry { far2.sqrt() as f32 } else { r_eq };
        if idx.insert(Bldg {
            x: (r.e - origin.0) as f32,
            y: (r.n - origin.1) as f32,
            r: r_eq,
            br,
            top_masl: (r.ground_z + r.height_m) as f32,
            poly_start,
            poly_end,
        }) {
            suspect += 1;
        }
        idx.area_height.push([r.area_m2 as f32, r.height_m as f32]);
    }
    progress.parsed(read, idx.count);
    (idx, bad, suspect)
}

/// The index of the pack's building file: the saved one where it is
/// current, else the file parsed and the index saved for the next start.
/// `Err` only when the file cannot be read at all.
fn building_index(
    jsonl: &Path,
    origin: (f64, f64),
    progress: &IndexProgress,
) -> Result<BuildingIndex, String> {
    let started = std::time::Instant::now();
    let f = File::open(jsonl).map_err(|e| e.to_string())?;
    // Stamped from the handle about to be read, so the stamp is the file's
    // that the index is built from. A file whose mtime cannot be read is
    // parsed every time, never cached.
    let stamp = f.metadata().and_then(|m| FileStamp::of(&m)).ok();
    let saved = saved_index_path(jsonl);
    let suspect_note = |suspect: usize| {
        if suspect > 0 {
            println!("  {suspect} footprint(s) wider than 1 km — geometry ignored for those");
        }
    };
    progress.phase(IndexPhase::Reading);
    let loaded = match stamp {
        None => Err("the building file has no modification time".to_string()),
        Some(stamp) => match std::fs::read(&saved) {
            Ok(bytes) => BuildingIndex::from_bytes(&bytes, stamp, origin),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("none saved yet".into()),
            Err(e) => Err(e.to_string()),
        },
    };
    match loaded {
        Ok((mut idx, suspect)) => {
            idx.stamp = stamp;
            println!(
                "buildings: {} LoD2 record(s) loaded from {} in {:.3} s — {} with real \
                 footprints ({} vertices), {} as equal-area discs",
                idx.count,
                saved.display(),
                started.elapsed().as_secs_f64(),
                idx.with_geometry,
                idx.verts.len(),
                idx.count - idx.with_geometry
            );
            suspect_note(suspect);
            progress.done(true, idx.count);
            return Ok(idx);
        }
        Err(why) => {
            println!("buildings: no saved index to load ({why}); parsing {}", jsonl.display())
        }
    }
    progress.bytes_total.store(stamp.map_or(0, |s| s.bytes), Ordering::Relaxed);
    progress.phase(IndexPhase::Parsing);
    let (mut idx, bad, suspect) = parse_buildings(f, origin, progress);
    idx.stamp = stamp;
    // Say whether the footprints are REAL or approximated. A pack
    // built without --lod2-geometry answers every containment test
    // with an equal-area disc, which is a different and much coarser
    // claim than a polygon — and the two are indistinguishable from
    // the record count alone.
    println!(
        "buildings: {} LoD2 record(s) indexed in {:.1} s{} — {} with real \
         footprints ({} vertices), {} as equal-area discs",
        idx.count,
        started.elapsed().as_secs_f64(),
        if bad > 0 { format!(", {bad} unparseable") } else { String::new() },
        idx.with_geometry,
        idx.verts.len(),
        idx.count - idx.with_geometry
    );
    suspect_note(suspect);
    if let Some(stamp) = stamp {
        progress.phase(IndexPhase::Saving);
        match save_index(&saved, &idx, stamp) {
            Ok(()) => println!("buildings: index saved to {} for the next start", saved.display()),
            Err(e) => eprintln!(
                "buildings: could not save the index to {} ({e}); the next start parses \
                 the building file again",
                saved.display()
            ),
        }
    }
    progress.done(false, idx.count);
    Ok(idx)
}

/// An antenna inside a building: the building, and the entry loss its
/// signal pays to leave (or reach) it.
///
/// Such an antenna is at its own height, not lifted onto its roof: P.2108's
/// terminal correction is for a terminal in the open among clutter, and read
/// here it put a node on the ground inside the Fernsehturm at the tower's
/// 253 m. Its own building is taken out of its paths instead, since the path
/// starts inside it, and what the walls cost is P.2109's median building
/// entry loss for a traditional building, horizontally.
#[derive(Clone, Copy)]
struct Indoor {
    building: u32,
    entry_db: f64,
}

fn indoor_terminal(ix: &BuildingIndex, x: f64, y: f64, antenna_masl: f64, f_ghz: f64) -> Option<Indoor> {
    ix.indoor_at(x, y, antenna_masl).map(|building| Indoor {
        building,
        entry_db: planner_core::entry_loss::p2109_bel_db(
            f_ghz,
            0.0,
            0.5,
            planner_core::entry_loss::BuildingStock::Traditional,
        ),
    })
}

#[derive(Default)]
enum NetworkCensus {
    #[default]
    Idle,
    Computing {
        started: Option<std::time::Instant>,
    },
    Ready(Box<NetworkResult>),
    Failed(String),
}

struct NetworkResult {
    /// The census raster, one BYTE per cell.
    ///
    /// This is the longest-lived allocation the server makes: it is kept until
    /// the next census replaces it, i.e. for the rest of the process on a
    /// server nobody re-runs. On the Berlin 5 m pack that window is 98 M
    /// cells — 392 MB as the f32 `Grid` it used to be, 98 MB as counts. The
    /// wire format `/network.bin` already spoke was u8; only the server's own
    /// copy was four times wider than the numbers in it.
    ///
    /// Shared, so the footprint routes read values from it without holding
    /// the census lock while they build a reply.
    served: Arc<planner_coverage::gaps::CountGrid>,
    k_target: u8,
    sites: usize,
    skipped: usize,
    assumed_height_m: f64,
    rx_situation: String,
    budget_db: f32,
    total_pop: f64,
    served_pop: f64,
    served_k_pop: f64,
    uncovered_pop: f64,
    uniform_weights: bool,
    gaps: Vec<(f64, f64, f64, f64, f64)>, // lon, lat, uncovered, km2, nearest_m
    backbone_components: usize,
    backbone_isolated: usize,
    elapsed_ms: u128,
}

struct CachedCoverage {
    key: CoverageKey,
    /// Shared, as the census raster is, for the footprint routes.
    loss: Arc<planner_terrain::Grid>,
    compute_ms: u128,
    /// The caller's name for the sweep this band is of, when it gave one.
    tag: Option<Arc<SweepTag>>,
}

/// A sweep a caller named, and the raster it will ask `/loss.bin` for once
/// the sweep is whole: sim-mesh's front caches a node's coverage by its key,
/// and while the ladder grows, `/coverage/bands.bin` reads the node's
/// coverage out of the band finished last, cut and sampled as that raster
/// will be.
struct SweepTag {
    key: String,
    view: ViewRect,
    w: u32,
    h: u32,
}

/// Inverse-project one pack-CRS point to `(lon, lat)` in degrees.
///
/// Free function rather than a method so the projection can be tested without
/// standing up an AppState (which needs an open COG). The tuple order is
/// `(lon, lat)` — proj4rs works in x/y, and every caller here destructures it
/// as `(lon, lat)`; swapping the two would put every exported node in the
/// Indian Ocean, which is the kind of mistake only a test catches.
fn to_lonlat_with(utm: &proj4rs::Proj, ll: &proj4rs::Proj, p: Xy) -> (f64, f64) {
    let mut pt = (p.x, p.y, 0.0);
    proj4rs::transform::transform(utm, ll, &mut pt).ok();
    (pt.0.to_degrees(), pt.1.to_degrees())
}

impl AppState {
    fn to_lonlat(&self, p: Xy) -> (f64, f64) {
        to_lonlat_with(&self.utm, &self.ll, p)
    }
    fn to_xy(&self, lat: f64, lon: f64) -> Xy {
        let mut pt = (lon.to_radians(), lat.to_radians(), 0.0);
        proj4rs::transform::transform(&self.ll, &self.utm, &mut pt).ok();
        Xy { x: pt.0, y: pt.1 }
    }
    /// Clamp a world point into the pack, so windowing never fails.
    fn clamp(&self, p: Xy) -> Xy {
        Xy {
            x: p.x.clamp(self.extent.min_x, self.extent.max_x),
            y: p.y.clamp(self.extent.min_y, self.extent.max_y),
        }
    }
}

/// Pack cell size in metres (layers share one grid by contract).
/// Hard ceiling on radials in one sweep. A guard against a pathological
/// request, NOT a resolution policy — see [`sweep_azimuths`].
const MAX_SWEEP_AZIMUTHS: usize = 32_768;

/// Radials to sweep for a `radius_m` circle on a `cell_m` raster.
///
/// The requirement is geometric: adjacent radials diverge as 2πR/N, so to
/// keep the ANGULAR resolution at least as fine as the raster out to the rim
/// you need N ≥ 2πR/cell. Below that the sweep is sampling the city along a
/// few thousand lines and interpolating between them, and the interpolation
/// shows up as spokes — an artefact of the sweep geometry that survives any
/// amount of terrain detail, and that reads to a viewer as structure.
///
/// This used to be capped at 1440, which on a 5 m pack meant a 20 km sweep
/// asked for 25 133 radials and got 87 m of arc spacing at the rim: 17 cells
/// between adjacent samples. A Berlin street is about 20 m wide, so street
/// structure was not merely unresolved, it could not be represented.
fn sweep_azimuths(radius_m: f64, cell_m: f64, requested: Option<usize>) -> usize {
    let ideal = (2.0 * std::f64::consts::PI * radius_m / cell_m.max(1.0)).ceil() as usize;
    requested.unwrap_or(ideal).clamp(180, MAX_SWEEP_AZIMUTHS)
}

fn terrain_res_hint(st: &AppState) -> f64 {
    st.manifest
        .layers
        .iter()
        .find_map(|l| l.res_m)
        .unwrap_or(30.0)
}

fn png_bytes(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h) as usize);
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        // Fast compression: these are generated per interaction, and the
        // transport is loopback — spending CPU on bytes is the wrong trade.
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(rgba).expect("png data");
    }
    out
}

#[derive(Deserialize)]
struct ViewQuery {
    minx: f64,
    miny: f64,
    maxx: f64,
    maxy: f64,
    w: u32,
    h: u32,
    #[serde(default)]
    layer: String,
}

async fn view_png(State(st): State<Arc<AppState>>, Query(q): Query<ViewQuery>) -> Response {
    // Degenerate sizes are clamped rather than rejected: a transient layout
    // measurement should never blank the map with an error.
    let (rw, rh) = (q.w.clamp(1, 8192), q.h.clamp(1, 8192));
    if rw * rh > MAX_PIXELS {
        return (StatusCode::BAD_REQUEST, "requested image exceeds the pixel cap").into_response();
    }
    // Shed load rather than thrash: if all render slots are busy, say so and
    // let the client retry on its next frame.
    let Ok(_permit) = st.slots.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "renderer busy").into_response();
    };

    let view = ViewRect { min_x: q.minx, min_y: q.miny, max_x: q.maxx, max_y: q.maxy };
    let (base, tint_layer) = match q.layer.as_str() {
        "clutter" => (BaseLayer::HillshadeClutter, st.layers.clutter.as_ref()),
        "population" => (BaseLayer::HillshadePopulation, st.layers.population.as_ref()),
        _ => (BaseLayer::Hillshade, None),
    };
    let opts = RenderOpts { width: rw, height: rh, base, ..Default::default() };

    // Read one window per layer, sized to the request.
    let lo = st.clamp(Xy { x: view.min_x, y: view.min_y });
    let hi = st.clamp(Xy { x: view.max_x, y: view.max_y });
    let (terrain, tint, classes) = tokio::task::block_in_place(|| {
        (
            st.layers.terrain.window(lo, hi),
            tint_layer.and_then(|l| l.window(lo, hi).ok()),
            st.layers.classes.as_ref().and_then(|l| l.window(lo, hi).ok()),
        )
    });
    let terrain = match terrain {
        Ok(g) => g,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut rgba = planner_render::render_base_with_classes(
        &terrain,
        tint.as_ref(),
        classes.as_ref(),
        &view,
        &opts,
    );
    draw_roads_into(&st, &mut rgba, &view, &opts);
    let png = png_bytes(&rgba, rw, rh);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        png,
    )
        .into_response()
}

/// Draw only the ways whose bounding box meets the view — at region scale
/// most of the network is off-screen and must not be walked per frame.
fn draw_roads_into(st: &AppState, rgba: &mut [u8], view: &ViewRect, opts: &RenderOpts) {
    if st.roads.is_empty() {
        return;
    }
    let lines: Vec<planner_render::RoadLine> = st
        .roads
        .iter()
        .filter(|(_, bb)| {
            (bb[0] as f64) <= view.max_x
                && (bb[2] as f64) >= view.min_x
                && (bb[1] as f64) <= view.max_y
                && (bb[3] as f64) >= view.min_y
        })
        .map(|(w, _)| planner_render::RoadLine { class: w.class as u8, points: &w.points })
        .collect();
    planner_render::draw_roads(rgba, view, opts, &lines);
}

#[derive(Deserialize)]
struct CoverageQuery {
    lat: f64,
    lon: f64,
    #[serde(default = "d_tx_h")]
    tx_h: f64,
    #[serde(default = "d_rx_h")]
    rx_h: f64,
    #[serde(default = "d_radius")]
    radius_km: f64,
    #[serde(default = "d_budget")]
    budget_db: f32,
    // View to composite into.
    minx: f64,
    miny: f64,
    maxx: f64,
    maxy: f64,
    w: u32,
    h: u32,
    #[serde(default)]
    layer: String,
}
fn d_tx_h() -> f64 {
    20.0
}
fn d_rx_h() -> f64 {
    2.0
}
fn d_radius() -> f64 {
    // Compute cap, not range: it must sit at or beyond the radio horizon or
    // it truncates real links (a 12.8 km path is invisible under an 8 km cap).
    20.0
}
fn d_budget() -> f32 {
    147.5
}

async fn coverage_png(State(st): State<Arc<AppState>>, Query(q): Query<CoverageQuery>) -> Response {
    // Degenerate sizes are clamped rather than rejected: a transient layout
    // measurement should never blank the map with an error.
    let (rw, rh) = (q.w.clamp(1, 8192), q.h.clamp(1, 8192));
    if rw * rh > MAX_PIXELS {
        return (StatusCode::BAD_REQUEST, "requested image exceeds the pixel cap").into_response();
    }
    let radius_m = q.radius_km.clamp(0.5, MAX_COVERAGE_RADIUS_KM) * 1000.0;
    let Ok(_permit) = st.sweep_slots.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "a propagation sweep is already running; this one would not fit in memory beside it").into_response();
    };

    let tx = st.to_xy(q.lat, q.lon);
    let view = ViewRect { min_x: q.minx, min_y: q.miny, max_x: q.maxx, max_y: q.maxy };
    let (base, tint_layer) = match q.layer.as_str() {
        "clutter" => (BaseLayer::HillshadeClutter, st.layers.clutter.as_ref()),
        "population" => (BaseLayer::HillshadePopulation, st.layers.population.as_ref()),
        _ => (BaseLayer::Hillshade, None),
    };
    let opts = RenderOpts { width: rw, height: rh, base, ..Default::default() };

    // Terrain/clutter windows big enough for BOTH the view and the sweep.
    let lo = st.clamp(Xy {
        x: view.min_x.min(tx.x - radius_m),
        y: view.min_y.min(tx.y - radius_m),
    });
    let hi = st.clamp(Xy {
        x: view.max_x.max(tx.x + radius_m),
        y: view.max_y.max(tx.y + radius_m),
    });
    let (terrain, clutter, tint) = tokio::task::block_in_place(|| {
        (
            st.layers.terrain.window(lo, hi),
            st.layers.clutter.as_ref().and_then(|l| l.window(lo, hi).ok()),
            tint_layer.and_then(|l| l.window(lo, hi).ok()),
        )
    });
    let terrain = match terrain {
        Ok(g) => g,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let mut link = planner_core::model::LinkParams::eu868_defaults();
    link.tx_h_agl_m = q.tx_h.clamp(1.0, 300.0);
    link.rx_h_agl_m = q.rx_h.clamp(1.0, 100.0);
    link.delta_n = st.manifest.region.delta_n;
    link.n0 = st.manifest.region.n0;
    link.path_center_lat_deg = q.lat;

    // Azimuth count is a property of the COMPUTATION, deliberately not of
    // the view: making it depend on zoom would invalidate the cache on every
    // navigation, which is the opposite of controlling when heavy work runs.
    // ~1 ray per raster cell at the rim, capped — beyond that the extra rays
    // land inside the same pixels.
    let cell_m = terrain_res_hint(&st).max(1.0);
    let az = sweep_azimuths(radius_m, cell_m, None);

    // Heavy work only when the transmitter or link settings actually change.
    // Navigation reuses the cached loss raster and just re-composites.
    let key = CoverageKey {
        lat: q.lat,
        lon: q.lon,
        tx_h: link.tx_h_agl_m,
        rx_h: link.rx_h_agl_m,
        radius_km: radius_m / 1000.0,
        budget_db: q.budget_db,
        azimuths: az,
    };
    // Check the cache, then RELEASE it before sweeping. This handler held the
    // coverage_cache mutex for its whole run -- tens of seconds at 20 km --
    // and /loss.bin and /buildings.bin take that same mutex as their first
    // act, so one scripted /coverage.png froze the page's overlays for the
    // length of a sweep. The page never calls this route; a script might, and
    // the page must not pay for it. The guard is re-taken only to publish.
    let hit = st.coverage_cache.lock().await.as_ref().is_some_and(|c| c.key == key);
    if !hit {
        let t0 = std::time::Instant::now();
        // Inside the SWEEP pool: on the global one this fans 25 000 azimuth
        // tasks into the same queue a tile's `rayon::join` uses, and the tile
        // waits behind every one of them.
        let cov = tokio::task::block_in_place(|| {
            st.sweep_pool.install(|| planner_coverage::coverage(
                &terrain,
                clutter.as_ref(),
                &planner_coverage::CoverageParams {
                    tx,
                    radius_m,
                    link,
                    // A single PNG render; nothing supersedes it.
                    cancel: None,
                    tx_terminal_db: None,
                    tx_model_h_m: None,
                    rx_terminal: None,
                    max_azimuths: Some(az),
                    stop_above_db: Some(q.budget_db + 18.0),
                    stop_after_m: planner_coverage::DEFAULT_STOP_AFTER_M,
                    max_profile_points: planner_coverage::MAX_PROFILE_POINTS,
                    table_budget_bytes: planner_coverage::DEFAULT_TABLE_BUDGET_BYTES,
                },
            ))
        });
        match cov {
            Ok(c) => {
                *st.coverage_cache.lock().await = Some(CachedCoverage {
                    key,
                    loss: Arc::new(c.loss),
                    compute_ms: t0.elapsed().as_millis(),
                    tag: None,
                })
            }
            // A transmitter outside the pack (or any other sweep refusal) is
            // a normal navigation state, not a client error: return the map
            // without an overlay rather than a 400 that blanks the view.
            Err(e) => {
                let mut rgba = planner_render::render_base_with_classes(
                    &terrain, tint.as_ref(), None, &view, &opts,
                );
                draw_roads_into(&st, &mut rgba, &view, &opts);
                let png = png_bytes(&rgba, rw, rh);
                let mut resp = (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "image/png"),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    png,
                )
                    .into_response();
                if let Ok(v) = axum::http::HeaderValue::from_str(&e.to_string()) {
                    resp.headers_mut().insert("x-coverage-skipped", v);
                }
                return resp;
            }
        }
    }
    let cache = st.coverage_cache.lock().await;
    let cached = cache.as_ref().expect("populated above");
    let ms = if hit { 0 } else { cached.compute_ms };
    let loss = &cached.loss;

    let classes = tokio::task::block_in_place(|| {
        st.layers.classes.as_ref().and_then(|l| l.window(lo, hi).ok())
    });
    let mut rgba = planner_render::render_base_with_classes(
        &terrain,
        tint.as_ref(),
        classes.as_ref(),
        &view,
        &opts,
    );
    planner_render::overlay_coverage(&mut rgba, loss, &view, &opts, q.budget_db);
    // Roads go ON TOP of coverage: the overlay is translucent, and the point
    // of the streets is to locate the coverage against known geography.
    draw_roads_into(&st, &mut rgba, &view, &opts);
    planner_render::draw_marker(&mut rgba, &view, &opts, tx, 5, [200, 30, 45]);
    let png = png_bytes(&rgba, rw, rh);
    let mut resp = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        png,
    )
        .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&ms.to_string()) {
        resp.headers_mut().insert("x-compute-ms", v);
    }
    resp
}

#[derive(Deserialize)]
struct TileQuery {
    minx: f64,
    miny: f64,
    maxx: f64,
    maxy: f64,
    /// Requested cell count across — the LOD knob. The server downsamples to
    /// this, so transfer is bounded by SCREEN size, not by area: zooming out
    /// covers more ground at coarser detail for the same bytes.
    w: u32,
    h: u32,
    /// Non-zero: send the TERRAIN block only.
    ///
    /// The land-cover, population and clutter blocks exist so the browser can
    /// bake the base map and switch layers locally. With `/basemap.bin` doing
    /// the bake, the page holds this tile only for its geometry — so the other
    /// three blocks are 4 bytes per cell it will never read, and, worse, three
    /// more windowed reads competing for the same layer locks as the basemap
    /// request running beside it.
    #[serde(default)]
    terrain_only: u8,
}

/// Cells to actually send for a request of `want_w` × `want_h` over `view`.
///
/// The client asks in SCREEN pixels — 1.6× the canvas, so small pans need no
/// new data. Zoomed in, that is far finer than the pack: a 2 km view at 1920
/// px asks for a 1.04 m cell from a 5 m pack, so 24 of every 25 cells sent are
/// interpolation the browser could have done itself. Measured on the Berlin
/// 5 m pack, that request returned 16.1 MB where 0.65 MB carried information,
/// on every pan.
///
/// So never send finer than the source. Nothing is lost: the reply carries its
/// own `res_x`/`res_y` and the WASM renderer already bilinearly interpolates
/// the tile to the canvas, so the picture is the same one — it is only the
/// upsampling that moves off the wire and into the client, where it is free.
///
/// The 2048 cap stays as the absolute ceiling for zoomed-OUT views, where the
/// pack is finer than the screen and the limit is the screen.
/// `native_x`/`native_y` are the SOURCE's cell size on each axis. Passing one
/// figure for both is wrong for any raster whose cells are not square: the
/// vertical cap is then computed against the horizontal step, so a source
/// finer north-south than east-west is truncated and a coarser one is sent
/// interpolated. Pack layers happen to be square today, but `window_raster`
/// emits an independent `res_y`, so the asymmetry is already representable.
fn tile_dims(
    view: &ViewRect,
    want_w: u32,
    want_h: u32,
    native_x: f64,
    native_y: f64,
) -> (u32, u32) {
    let cap = |want: u32, span_m: f64, native_m: f64| -> u32 {
        let native_cells = if native_m.is_finite() && native_m > 0.0 {
            (span_m / native_m).ceil().clamp(16.0, 2048.0) as u32
        } else {
            2048
        };
        want.clamp(16, 2048).min(native_cells)
    };
    (cap(want_w, view.width_m(), native_x), cap(want_h, view.height_m(), native_y))
}

/// How to read a cached raster when resampling it for a view.
///
/// Not a quality knob. Each variant matches what the corresponding overlay in
/// `planner-render` already does per screen pixel, so that resampling here
/// draws the same picture the browser would have drawn from the full raster.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Resample {
    /// Loss, a continuous field in dB — `overlay_coverage` reads it bilinearly.
    Bilinear,
    /// Census counts — `overlay_network` reads them NEAREST on purpose:
    /// averaging 0 and 2 repeaters into 1 invents a redundancy tier that no
    /// site provides. Resampling them any other way here would reintroduce
    /// exactly that.
    Nearest,
}

/// What `window_raster` needs of a source raster: its geometry, and one sample
/// at a world point.
///
/// It exists because the two rasters that go over the wire no longer have the
/// same element type — the loss raster is f32 dB, the census raster is u8
/// counts (`planner_coverage::gaps::CountGrid`, 98 MB instead of 392 MB on the
/// Berlin 5 m pack). Windowing is the same arithmetic for both, and it is
/// arithmetic with three separate half-cell conventions in it, so a second
/// copy of it for the census would be a second place for a misregistration to
/// hide. The window itself is at most 2048x2048, so materialising it as f32
/// and packing it back down in the handler costs 16 MB transiently and keeps
/// one clipping routine.
trait ViewSource {
    fn origin(&self) -> Xy;
    fn dx_m(&self) -> f64;
    fn dy_m(&self) -> f64;
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    /// `None` outside the raster; NaN inside it where there is no value.
    fn sample(&self, p: Xy, how: Resample) -> Option<f32>;
}

impl ViewSource for planner_terrain::Grid {
    fn origin(&self) -> Xy {
        self.origin
    }
    fn dx_m(&self) -> f64 {
        self.dx_m
    }
    fn dy_m(&self) -> f64 {
        self.dy_m
    }
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
    fn sample(&self, p: Xy, how: Resample) -> Option<f32> {
        match how {
            Resample::Bilinear => self.sample_bilinear(p),
            Resample::Nearest => self.sample_nearest(p),
        }
    }
}

impl ViewSource for planner_coverage::gaps::CountGrid {
    fn origin(&self) -> Xy {
        self.origin
    }
    fn dx_m(&self) -> f64 {
        self.dx_m
    }
    fn dy_m(&self) -> f64 {
        self.dy_m
    }
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
    /// Counts are read NEAREST whatever is asked for. Averaging 0 and 2
    /// repeaters into 1 invents a redundancy tier no site provides, and a
    /// caller that asked for bilinear here has made a mistake this raster can
    /// answer correctly rather than propagate.
    fn sample(&self, p: Xy, _how: Resample) -> Option<f32> {
        self.sample_nearest(p)
    }
}

/// One cached raster, resampled onto the current view at the resolution the
/// screen can actually show.
///
/// WHY THIS EXISTS. `/loss.bin` and `/network.bin` shipped the WHOLE raster
/// and let the browser hold it. Measured on the Berlin 5 m pack:
///
/// | payload            | cells  | wire   | to decode | held in the tab |
/// |--------------------|--------|--------|-----------|-----------------|
/// | 20 km sweep        |  64 M  | 128 MB |   ~4.1 s  |          512 MB |
/// | 30 km sweep        | 144 M  | 288 MB |   ~9.2 s  |          1.2 GB |
/// | network census     |  98 M  |  98 MB |    1.2 s  |          1.3 GB |
///
/// The sweep figure is per BAND — six bands, growing as r², so about 1.7× the
/// last one, all of it on the main thread. The census was measured pushing
/// WASM linear memory from 1 MB to 786 MB across the detail tile and the
/// overview, and it is re-pushed into every new tile, i.e. on every pan.
///
/// None of those bytes could be seen. The browser draws both layers by
/// sampling them ONCE PER SCREEN PIXEL, so everything finer than a pixel and
/// everything off-screen was transferred in order to be sampled away. Doing
/// that sampling here, with the same sampler at the same points, sends the
/// picture the client was going to draw and nothing else.
///
/// This is a TRANSPORT change, not a model change: the full-resolution raster
/// stays on the server, the census and the per-building values are still
/// computed from it, and zooming in fetches the native cells for the smaller
/// window. Accuracy is unchanged; only the bytes that were never going to
/// reach a pixel are gone.
fn window_raster<S: ViewSource + Sync + ?Sized>(
    src: &S,
    view: &ViewRect,
    want_w: u32,
    want_h: u32,
    how: Resample,
) -> (u32, u32, f64, f64, Xy, Vec<f32>) {
    let (w, h) = tile_dims(view, want_w, want_h, src.dx_m().abs(), src.dy_m().abs());
    window_cells(src, view, w, h, how)
}

/// `window_raster` on exactly `w`×`h` cells over `view`, however much finer
/// than the source's own they are: for a caller whose picture is drawn a
/// cell of its own at a time, the coverage bands.
fn window_cells<S: ViewSource + Sync + ?Sized>(
    src: &S,
    view: &ViewRect,
    w: u32,
    h: u32,
    how: Resample,
) -> (u32, u32, f64, f64, Xy, Vec<f32>) {
    let res_x = view.width_m() / w as f64;
    let res_y = view.height_m() / h as f64;

    // Clip to where the raster actually HAS data, at the view's own cell size.
    //
    // A 2 km sweep looked at from a 25 km view is 4 km of answer inside 25 km
    // of "not evaluated", and sending the margin costs the same per cell as
    // sending the answer: measured 6.24 MB, of which 0.2 MB carried anything.
    // The reply already states its own origin, so a smaller rectangle needs no
    // agreement with the client beyond the one it already has.
    //
    // The grid step stays the VIEW's, not the source's, so the clipped window
    // lands on exactly the cells the unclipped one would have — clipping moves
    // the edges, never the samples.
    let (sdx, sdy) = (src.dx_m().abs(), src.dy_m().abs());
    let (sx0, sx1) =
        (src.origin().x - 0.5 * sdx, src.origin().x + (src.width() as f64 - 0.5) * sdx);
    let (sy1, sy0) =
        (src.origin().y + 0.5 * sdy, src.origin().y - (src.height() as f64 - 0.5) * sdy);
    let c_lo = ((sx0 - view.min_x) / res_x - 0.5).ceil().max(0.0) as i64;
    let c_hi = ((sx1 - view.min_x) / res_x - 0.5).floor().min(w as f64 - 1.0) as i64;
    let r_lo = ((view.max_y - sy1) / res_y - 0.5).ceil().max(0.0) as i64;
    let r_hi = ((view.max_y - sy0) / res_y - 0.5).floor().min(h as f64 - 1.0) as i64;
    if c_hi < c_lo || r_hi < r_lo {
        // No overlap at all. One NaN cell rather than a zero-sized grid: an
        // empty `Grid` underflows `width - 1` inside the samplers, and a
        // special case that only fires when the operator pans off the swept
        // area is a special case that gets tested by an operator.
        return (
            1,
            1,
            res_x,
            res_y,
            Xy { x: view.min_x + 0.5 * res_x, y: view.max_y - 0.5 * res_y },
            vec![f32::NAN],
        );
    }
    let (c_lo, r_lo) = (c_lo as usize, r_lo as usize);
    let (ow, oh) = ((c_hi as usize - c_lo + 1) as u32, (r_hi as usize - r_lo + 1) as u32);

    // A Grid's origin is the CENTRE of cell (0,0) — `sample_bilinear` maps it
    // to fx = 0 — while `px_to_world` returns pixel CENTRES. The half-cell
    // offset is what makes the two agree; without it every overlay sits half
    // a cell north-west of the terrain it is explaining, which reads as the
    // coverage being misregistered rather than as an off-by-one.
    let origin = Xy {
        x: view.min_x + (c_lo as f64 + 0.5) * res_x,
        y: view.max_y - (r_lo as f64 + 0.5) * res_y,
    };
    let mut data = vec![f32::NAN; (ow as usize) * (oh as usize)];
    data.par_chunks_mut(ow as usize).enumerate().for_each(|(row, line)| {
        for (col, v) in line.iter_mut().enumerate() {
            let p = view.px_to_world((c_lo + col) as u32, (r_lo + row) as u32, w, h);
            *v = src.sample(p, how).unwrap_or(f32::NAN);
        }
    });
    (ow, oh, res_x, res_y, origin, data)
}

/// The view a raster request is asking for, when it asks for one.
///
/// Absent means "the whole raster", which is how scripted callers and the
/// tests that assert the sweep itself still reach the unresampled numbers —
/// a resample must not be able to stand in for the model when something is
/// checking the model.
fn requested_view(
    minx: Option<f64>,
    miny: Option<f64>,
    maxx: Option<f64>,
    maxy: Option<f64>,
) -> Option<ViewRect> {
    let (a, b, c, d) = (minx?, miny?, maxx?, maxy?);
    let v = ViewRect { min_x: a.min(c), min_y: b.min(d), max_x: a.max(c), max_y: b.max(d) };
    (v.width_m() > 0.0 && v.height_m() > 0.0).then_some(v)
}

/// Nearest-cell value of a raster at a world point; NaN off the raster.
///
/// One sample at the CENTROID, which for a courtyard block lands in the yard
/// rather than on the building. Accepted because the alternative -- averaging
/// over the footprint -- is the cell-mean mistake one level up, and a single
/// honest sample beats a blend of roof and yard.
fn grid_value_at(g: &planner_terrain::Grid, x: f64, y: f64) -> f32 {
    let col = ((x - g.origin.x) / g.dx_m).round();
    let row = ((g.origin.y - y) / g.dy_m.abs()).round();
    if col < 0.0 || row < 0.0 {
        return f32::NAN;
    }
    let (c, r) = (col as usize, row as usize);
    if c >= g.width || r >= g.height {
        return f32::NAN;
    }
    g.data[r * g.width + c]
}

/// The two per-building answers the map can colour by, for one building.
///
/// `loss` from the single-site sweep and `census` (how many deployed
/// repeaters reach it) from the whole-mesh census. Either is NaN when that
/// computation has not run, and the page draws NaN as "not evaluated", never
/// as zero. Both are sampled from the full-resolution rasters the server
/// holds, not from the windowed copies the page has -- the page never sees a
/// value that was resampled for display.
fn building_values(
    loss: Option<&planner_terrain::Grid>,
    census: Option<&planner_coverage::gaps::CountGrid>,
    x: f64,
    y: f64,
) -> (f32, f32) {
    (
        loss.map_or(f32::NAN, |g| grid_value_at(g, x, y)),
        // Same nearest-cell rule as `grid_value_at`, from the census raster's
        // own accessor: it is the one that knows 255 means "not evaluated".
        census.map_or(f32::NAN, |g| g.count_at_world(x, y)),
    )
}

/// `/buildings.bin`'s query: the box, and whether to fill in each building's
/// values.
#[derive(Deserialize)]
struct BuildingsQuery {
    minx: f64,
    miny: f64,
    maxx: f64,
    maxy: f64,
    /// Non-zero: each ring carries its building's loss and census values,
    /// from the latest sweep band and the census. Otherwise both are NaN,
    /// "not evaluated". Off by default: sim-mesh's map draws footprints by
    /// height and never reads them, and computing them tied every reply to
    /// the sweep and census locks and to state no cache could validate.
    #[serde(default)]
    values: u8,
}

/// A hard vertex budget for one `/buildings.bin` reply rather than a zoom
/// rule. Berlin has 8.4 M vertices; a whole-city request would be hundreds of
/// megabytes and the outlines would be invisible anyway. The reply says when
/// it truncated so the page can show that it is not drawing everything,
/// instead of quietly drawing half a city.
const MAX_FOOTPRINT_VERTS: usize = 900_000;

/// The rasters per-building values are read from, as they are now: the
/// latest sweep band and the census. Each is taken from under its lock and
/// the lock let go at once, so a reply is built while sweeps and the census
/// publish beside it, and replies are built side by side.
async fn value_rasters(
    st: &AppState,
) -> (Option<Arc<planner_terrain::Grid>>, Option<Arc<planner_coverage::gaps::CountGrid>>) {
    let loss = st.coverage_cache.lock().await.as_ref().map(|c| Arc::clone(&c.loss));
    let census = match &*st.network.lock().await {
        NetworkCensus::Ready(r) => Some(Arc::clone(&r.served)),
        _ => None,
    };
    (loss, census)
}

/// The validator of a `/buildings.bin` reply without values: a hash of all
/// it is made from, which is the building file the index was built from (its
/// size and mtime), the grid the coordinates are relative to, the box and
/// the vertex budget. `None` for an index whose file had no mtime.
fn footprints_etag(ix: &BuildingIndex, lo: Xy, hi: Xy) -> Option<String> {
    let stamp = ix.stamp?;
    let words = [
        u64::from(INDEX_VERSION),
        stamp.bytes,
        stamp.mtime_s,
        u64::from(stamp.mtime_ns),
        ix.origin.0.to_bits(),
        ix.origin.1.to_bits(),
        lo.x.to_bits(),
        lo.y.to_bits(),
        hi.x.to_bits(),
        hi.y.to_bits(),
        MAX_FOOTPRINT_VERTS as u64,
    ];
    // FNV-1a: the same on every build and machine, so a page keeps its
    // validators across sidecar restarts, which std's hasher does not
    // promise.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in b"PBO3".iter().copied().chain(words.iter().flat_map(|w| w.to_le_bytes())) {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    Some(format!("\"pbo3-{h:016x}\""))
}

/// Whether a request's `If-None-Match` names `etag`, or is `*`.
fn etag_matches(headers: &axum::http::HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
}

/// `resp` with its validator: an ETag and `no-cache`, so a browser keeps the
/// reply and asks whether it changed; `no-store` where there is none.
fn with_validator(mut resp: Response, etag: Option<&str>) -> Response {
    use axum::http::HeaderValue;
    let h = resp.headers_mut();
    match etag.and_then(|e| HeaderValue::from_str(e).ok()) {
        Some(v) => {
            h.insert(header::ETAG, v);
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        }
        None => {
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
    }
    resp
}

/// Footprint outlines in a box, for the page to draw and to place nodes on.
///
/// Built under no lock: the index is taken from under its own and the lock
/// let go, and values are read from rasters taken the same way. These
/// replies once held the sweep and census locks while they were built, so
/// the squares of a map were built one at a time, and a sweep band waited
/// for them to publish.
async fn buildings_bin(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BuildingsQuery>,
) -> Response {
    let lo = Xy { x: q.minx.min(q.maxx), y: q.miny.min(q.maxy) };
    let hi = Xy { x: q.minx.max(q.maxx), y: q.miny.max(q.maxy) };
    let Some(ix) = st.buildings.read().expect("buildings lock").ready() else {
        return (StatusCode::NO_CONTENT, "no building geometry in this pack").into_response();
    };
    if q.values == 0 {
        // Geometry alone is a function of the index and the box, so a page
        // that has drawn this box asks whether it changed instead of
        // fetching it again.
        let etag = footprints_etag(&ix, lo, hi);
        if etag.as_deref().is_some_and(|e| etag_matches(&headers, e)) {
            return with_validator(StatusCode::NOT_MODIFIED.into_response(), etag.as_deref());
        }
        let out = tokio::task::block_in_place(|| footprints_reply(&ix, lo, hi, None, None));
        let resp = ([(header::CONTENT_TYPE, "application/octet-stream")], out).into_response();
        return with_validator(resp, etag.as_deref());
    }
    let (loss, census) = value_rasters(&st).await;
    let out = tokio::task::block_in_place(|| {
        footprints_reply(&ix, lo, hi, loss.as_deref(), census.as_deref())
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream"), (header::CACHE_CONTROL, "no-store")],
        out,
    )
        .into_response()
}

/// `/buildings.bin`'s reply for a box: each footprint ring in it, carrying
/// its building's values read from `loss` and `census`, NaN where there is
/// no raster. Blocking.
///
/// Layout: "PBO3" | u32 ring_count | u8 truncated | f64 origin_x |
/// f64 origin_y | per ring: u32 building_id, u32 vertex_count, f32 top_masl,
/// f32 loss_db, f32 census_count, then vertex_count x (f32 dx, f32 dy).
///
/// Vertices are OFFSETS from the origin in the header. Sending absolute UTM
/// as f32 would quantise northings to 0.5 m -- the roads layer accepts that
/// because it only needs to look like a street, but this layer exists to
/// tell two adjacent buildings apart at a metre.
fn footprints_reply(
    ix: &BuildingIndex,
    lo: Xy,
    hi: Xy,
    loss: Option<&planner_terrain::Grid>,
    census: Option<&planner_coverage::gaps::CountGrid>,
) -> Vec<u8> {
    let (verts, lens, tops, ids, truncated) = ix.outlines_in(lo, hi, MAX_FOOTPRINT_VERTS);
    let mut out = Vec::with_capacity(25 + verts.len() * 8 + lens.len() * 20);
    out.extend_from_slice(b"PBO3");
    out.extend_from_slice(&(lens.len() as u32).to_le_bytes());
    out.push(u8::from(truncated));
    out.extend_from_slice(&ix.origin.0.to_le_bytes());
    out.extend_from_slice(&ix.origin.1.to_le_bytes());
    let mut v = 0usize;
    let mut last: Option<(u32, f32, f32)> = None;
    for (i, &n) in lens.iter().enumerate() {
        let id = ids[i];
        // Rings of one building are consecutive, so sample once per building.
        let (loss, census) = match last {
            Some((lid, l, c)) if lid == id => (l, c),
            _ => {
                let (x, y) = ix.centroid(id);
                let lc = building_values(loss, census, x, y);
                last = Some((id, lc.0, lc.1));
                lc
            }
        };
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&tops[i].to_le_bytes());
        out.extend_from_slice(&loss.to_le_bytes());
        out.extend_from_slice(&census.to_le_bytes());
        for p in &verts[v..v + n as usize] {
            out.extend_from_slice(&p[0].to_le_bytes());
            out.extend_from_slice(&p[1].to_le_bytes());
        }
        v += n as usize;
    }
    out
}

/// Values only -- no geometry -- for every building in a box.
///
/// WHY. A sweep band lands six times per sweep and the census finishes once,
/// and each of those changes one number per building. The page used to
/// refetch the whole footprint payload to learn it: 1.16 MB at 3 km and
/// 5.2 MB at 6 km of outlines it already held, decoded again, for 12 bytes
/// per building of actual news. This route is those 12 bytes.
///
/// Built under no lock, as `/buildings.bin` is.
///
/// Layout: "PBV1" | u32 count | per building: u32 id, f32 loss_db,
/// f32 census_count.
async fn buildings_values_bin(
    State(st): State<Arc<AppState>>,
    Query(q): Query<TileQuery>,
) -> Response {
    let lo = Xy { x: q.minx.min(q.maxx), y: q.miny.min(q.maxy) };
    let hi = Xy { x: q.minx.max(q.maxx), y: q.miny.max(q.maxy) };
    let Some(ix) = st.buildings.read().expect("buildings lock").ready() else {
        return (StatusCode::NO_CONTENT, "no building geometry in this pack").into_response();
    };
    let (loss, census) = value_rasters(&st).await;
    let out = tokio::task::block_in_place(|| {
        let ids = ix.ids_in(lo, hi);
        let mut out = Vec::with_capacity(8 + ids.len() * 12);
        out.extend_from_slice(b"PBV1");
        out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
        for id in ids {
            let (x, y) = ix.centroid(id);
            let (loss, census) = building_values(loss.as_deref(), census.as_deref(), x, y);
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&loss.to_le_bytes());
            out.extend_from_slice(&census.to_le_bytes());
        }
        out
    });
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        out,
    )
        .into_response()
}

/// Where the building index has got, while `/buildings.bin` answers 204 for
/// it: `state` is `/link.json`'s `buildings_index` (loading, ready, absent);
/// `phase` what the sidecar is doing; while it parses, `bytes_read` of
/// `bytes_total` of the building file; `source` once ready, the saved index
/// or the building file.
async fn buildings_status(State(st): State<Arc<AppState>>) -> Response {
    let state = st.buildings.read().expect("buildings lock").label();
    axum::Json(st.buildings_progress.json(state)).into_response()
}

/// One layer resampled onto a tile's `tw`×`th` cells: each cell's bilinear
/// sample at its centre, encoded by `enc` into `bpc` bytes.
///
/// Rows run in parallel. A screen-sized tile is ~1.8 M cells per layer and
/// four layers of serial bilinear sampling was the whole cost of a tile fetch
/// (~1.8 s), which in turn is what made refetching at a finer zoom level feel
/// expensive. The buffer is allocated once and each row writes only its own
/// slice, so no locking and no per-row allocation.
fn tile_block(
    g: &planner_terrain::Grid,
    view: &ViewRect,
    tw: u32,
    th: u32,
    bpc: usize,
    enc: &(dyn Fn(f32, &mut [u8]) + Sync),
) -> Vec<u8> {
    let mut buf = vec![0u8; (tw * th) as usize * bpc];
    buf.par_chunks_mut(tw as usize * bpc).enumerate().for_each(|(row, line)| {
        for col in 0..tw as usize {
            let p = view.px_to_world(col as u32, row as u32, tw, th);
            let v = g.sample_bilinear(p).unwrap_or(f32::NAN);
            enc(v, &mut line[col * bpc..col * bpc + bpc]);
        }
    });
    buf
}

/// Terrain as a tile carries it: decimetres, ±3200 m at 0.1 m, plenty for
/// any terrain, and `i16::MIN` where there is none.
fn terrain_decimetres(v: f32, dst: &mut [u8]) {
    let dm = if v.is_finite() { (v * 10.0).clamp(-32000.0, 32000.0) as i16 } else { i16::MIN };
    dst.copy_from_slice(&dm.to_le_bytes());
}

/// Compact binary tile: everything the browser needs to render this region
/// itself, fetched once and reused for every subsequent frame.
///
/// Layout (little-endian): "PTL1" | u32 w | u32 h | f64 origin_x | f64 origin_y
/// | f64 res_m | u8 flags(bit0 classes, bit1 population, bit2 clutter)
/// | i16 terrain[w*h] (decimetres) | u8 classes[w*h]? | u16 pop[w*h]? (×10 clamped)
/// | u16 clutter[w*h]? (decimetres)
async fn tile_bin(State(st): State<Arc<AppState>>, Query(q): Query<TileQuery>) -> Response {
    let view = ViewRect { min_x: q.minx, min_y: q.miny, max_x: q.maxx, max_y: q.maxy };
    let res_hint = terrain_res_hint(&st);
    let (tw, th) = tile_dims(&view, q.w, q.h, res_hint, res_hint);
    let Ok(_permit) = st.slots.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "renderer busy").into_response();
    };
    // Both axes, independently. Deriving one square `res` from the width and
    // applying it vertically as well silently stretches the raster whenever the
    // view aspect and the pixel aspect disagree (e.g. after a window resize) —
    // which is exactly how the base map drifts out from under the road overlay.
    let res_x = view.width_m() / tw as f64;
    let res_y = view.height_m() / th as f64;
    let lo = st.clamp(Xy { x: view.min_x, y: view.min_y });
    let hi = st.clamp(Xy { x: view.max_x, y: view.max_y });

    // Read the four layer windows CONCURRENTLY. They live in separate files,
    // so serialising them just added up four independent read costs — the
    // dominant part of a tile fetch, since a full-screen view can span half
    // the pack.
    //
    // The side layers only when their blocks will be sent: with
    // `terrain_only` the page never reads them. In and out the order is
    // classes, population, clutter, `None` for one this pack lacks.
    let want_side = q.terrain_only == 0;
    let side = [&st.layers.classes, &st.layers.population, &st.layers.clutter]
        .map(|l| l.as_ref().filter(|_| want_side));
    // Read at the STRIDE the reply needs, not at full resolution.
    //
    // `window` materialises the whole box: on this pack a zoomed-out request
    // covers all 10 742 × 9 127 cells, so one screen of map allocated 392 MB
    // per layer — 1.57 GB across four — to build a 3.2 M-cell tile that then
    // point-samples one cell in twenty-five. The stride is generous (2× the
    // output) because the resample below is bilinear and wants a neighbour on
    // each side; beyond that the extra cells were being read to be skipped.
    let (rw, rh) = ((tw as usize) * 2, (th as usize) * 2);
    let (terrain, side_out) = tokio::task::block_in_place(|| {
        rayon::join(
            || st.layers.terrain.window_max(lo, hi, rw, rh),
            || {
                side.par_iter()
                    .map(|l| l.and_then(|l| l.window_max(lo, hi, rw, rh).ok()))
                    .collect::<Vec<_>>()
            },
        )
    });
    let terrain = match terrain {
        Ok(g) => g,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let [classes, population, clutter]: [Option<planner_terrain::Grid>; 3] =
        side_out.try_into().expect("one window per side layer");

    let n = (tw * th) as usize;
    let mut out: Vec<u8> = Vec::with_capacity(16 + n * 6);
    out.extend_from_slice(b"PTL2");
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    // The CENTRE of cell (0,0), which is half a cell inside the box.
    //
    // `block` below samples at `view.px_to_world(col, row, ...)`, i.e. at
    // PIXEL CENTRES, so cell (0,0) carries the value at
    // `min_x + 0.5*res_x`. Declaring the corner instead told the browser that
    // value sits at `min_x`, and `Grid::sample_bilinear` reads `origin` as a
    // cell centre — so the whole base map was placed half a cell north-west of
    // the ground it was sampled from, while the coverage overlay (see
    // `window_raster`, which already offsets correctly) was placed right. Two
    // layers answering about different ground is the one thing this map must
    // not do.
    out.extend_from_slice(&(view.min_x + 0.5 * res_x).to_le_bytes());
    out.extend_from_slice(&(view.max_y - 0.5 * res_y).to_le_bytes());
    out.extend_from_slice(&res_x.to_le_bytes());
    out.extend_from_slice(&res_y.to_le_bytes());
    let flags = (classes.is_some() as u8) | ((population.is_some() as u8) << 1)
        | ((clutter.is_some() as u8) << 2);
    out.push(flags);
    let block = |g: &planner_terrain::Grid, bpc: usize, enc: &(dyn Fn(f32, &mut [u8]) + Sync)| {
        tile_block(g, &view, tw, th, bpc, enc)
    };
    let terrain_block = block(&terrain, 2, &terrain_decimetres);
    let class_block = classes.as_ref().map(|g| {
        block(g, 1, &|v, dst| {
            dst[0] = if v.is_finite() { v.round().clamp(0.0, 255.0) as u8 } else { 255 };
        })
    });
    let decimetre = |v: f32, dst: &mut [u8]| {
        let u = if v.is_finite() { (v * 10.0).clamp(0.0, 65535.0) as u16 } else { 0 };
        dst.copy_from_slice(&u.to_le_bytes());
    };
    let pop_block = population.as_ref().map(|g| block(g, 2, &decimetre));
    let clut_block = clutter.as_ref().map(|g| block(g, 2, &decimetre));

    out.extend_from_slice(&terrain_block);
    for b in [class_block, pop_block, clut_block].into_iter().flatten() {
        out.extend_from_slice(&b);
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        out,
    )
        .into_response()
}

#[derive(Deserialize)]
struct BasemapQuery {
    minx: f64,
    miny: f64,
    maxx: f64,
    maxy: f64,
    w: u32,
    h: u32,
    /// 0 terrain, 1 population, 2 clutter — the page's own layer codes.
    #[serde(default)]
    layer: u8,
    #[serde(default)]
    roads: u8,
}

/// The BAKED base image for a tile: hillshade, land cover, tint and roads,
/// rendered here instead of in the browser.
///
/// WHY THIS MOVED OFF THE CLIENT. The bake is the largest single piece of work
/// in the map and none of it depends on the camera, so `planner-wasm` computed
/// it once per tile and resampled it per frame. That was right, but "once per
/// tile" is still a 1.1-1.5 s block on the ONE thread the browser also uses to
/// process input — measured at 1075 ms without roads and 1351 ms with them on
/// a 1997x1600 tile, and it lands in the middle of a drag.
///
/// Here it is data-parallel over rows across every core (`planner-render`'s
/// `parallel` feature) and it is not on the browser's main thread at all: the
/// page fetches it, and a fetch does not block input. Measured on this pack,
/// the same view: 355 ms server-side against 1351 ms in the browser, and the
/// browser's share becomes a memcpy.
///
/// The image covers EXACTLY the requested box, at the same cell count
/// `tile_bin` would choose for it, so it registers with the tile's own grid
/// without either side restating the half-cell convention.
///
/// Layout: "PBM1" | u32 w | u32 h | f64 min_x | f64 min_y | f64 max_x |
/// f64 max_y | w*h*4 RGBA.
async fn basemap_bin(
    State(st): State<Arc<AppState>>,
    Query(q): Query<BasemapQuery>,
) -> Response {
    let view = ViewRect { min_x: q.minx, min_y: q.miny, max_x: q.maxx, max_y: q.maxy };
    if !(view.width_m() > 0.0 && view.height_m() > 0.0) {
        return (StatusCode::BAD_REQUEST, "degenerate box").into_response();
    }
    let res_hint = terrain_res_hint(&st);
    let (tw, th) = tile_dims(&view, q.w, q.h, res_hint, res_hint);
    let Ok(_permit) = st.slots.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "renderer busy").into_response();
    };
    let (base, tint_layer) = match q.layer {
        1 => (BaseLayer::HillshadePopulation, st.layers.population.as_ref()),
        2 => (BaseLayer::HillshadeClutter, st.layers.clutter.as_ref()),
        _ => (BaseLayer::Hillshade, None),
    };
    let lo = st.clamp(Xy { x: view.min_x, y: view.min_y });
    let hi = st.clamp(Xy { x: view.max_x, y: view.max_y });
    // Twice the output, as `tile_bin`: the render samples bilinearly and wants
    // a neighbour each side. See `CogReader::window_max`.
    let (rw, rh) = ((tw as usize) * 2, (th as usize) * 2);

    let (terrain, classes, tint) = tokio::task::block_in_place(|| {
        (
            st.layers.terrain.window_max(lo, hi, rw, rh),
            st.layers.classes.as_ref().and_then(|l| l.window_max(lo, hi, rw, rh).ok()),
            tint_layer.and_then(|l| l.window_max(lo, hi, rw, rh).ok()),
        )
    });
    let terrain = match terrain {
        Ok(g) => g,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let opts = RenderOpts { width: tw, height: th, base, ..Default::default() };
    let mut rgba = tokio::task::block_in_place(|| {
        let mut img = planner_render::render_base_with_classes(
            &terrain,
            tint.as_ref(),
            classes.as_ref(),
            &view,
            &opts,
        );
        if q.roads != 0 {
            let lines: Vec<planner_render::RoadLine> = st
                .roads
                .iter()
                .filter(|(_, bb)| {
                    !((bb[0] as f64) > view.max_x
                        || (bb[2] as f64) < view.min_x
                        || (bb[1] as f64) > view.max_y
                        || (bb[3] as f64) < view.min_y)
                })
                .map(|(w, _)| planner_render::RoadLine { class: w.class as u8, points: &w.points })
                .collect();
            planner_render::draw_roads(&mut img, &view, &opts, &lines);
        }
        img
    });

    let mut out = Vec::with_capacity(44 + rgba.len());
    out.extend_from_slice(b"PBM1");
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.extend_from_slice(&view.min_x.to_le_bytes());
    out.extend_from_slice(&view.min_y.to_le_bytes());
    out.extend_from_slice(&view.max_x.to_le_bytes());
    out.extend_from_slice(&view.max_y.to_le_bytes());
    out.append(&mut rgba);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        out,
    )
        .into_response()
}

/// Road geometry for a region, flattened for the WASM renderer:
/// [class, n, x0,y0, ...] repeated, as f32.
async fn roads_bin(State(st): State<Arc<AppState>>, Query(q): Query<TileQuery>) -> Response {
    let view = ViewRect { min_x: q.minx, min_y: q.miny, max_x: q.maxx, max_y: q.maxy };
    // A hard budget, as buildings.bin has. The page no longer bakes roads --
    // they arrive burnt into the basemap -- so this layer exists only for the
    // local-bake fallback, and a whole-pack view was still pulling every one
    // of 235 107 points (1.6 MB) into WASM memory that never shrinks. Ways are
    // kept whole, in the pack's order, until the budget is spent; the format
    // carries no header, so truncation is by omission, which the local bake
    // tolerates by drawing fewer minor roads.
    const MAX_ROAD_BYTES: usize = 2 * 1024 * 1024;
    let mut out: Vec<u8> = Vec::new();
    for (w, bb) in &st.roads {
        if out.len() + 8 + w.points.len() * 8 > MAX_ROAD_BYTES {
            break;
        }
        if (bb[0] as f64) > view.max_x
            || (bb[2] as f64) < view.min_x
            || (bb[1] as f64) > view.max_y
            || (bb[3] as f64) < view.min_y
        {
            continue;
        }
        out.extend_from_slice(&(w.class as u8 as f32).to_le_bytes());
        out.extend_from_slice(&(w.points.len() as f32).to_le_bytes());
        for (x, y) in &w.points {
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
        }
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        out,
    )
        .into_response()
}

/// The loss raster itself, so the browser can re-composite coverage locally
/// (budget slider, layer switches) without another sweep.
/// See `LinkQuery` for why unknown fields are rejected rather than ignored:
/// a misspelled `tx_h` here silently sweeps the whole city at the default
/// height.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct LossQuery {
    /// Projected pack coordinates. Preferred: the map clicks in this frame, so
    /// sending it directly means the sweep centre IS the pin. The older
    /// lat/lon form is kept for scripted callers.
    x: Option<f64>,
    y: Option<f64>,
    #[serde(default)]
    lat: f64,
    #[serde(default)]
    lon: f64,
    #[serde(default = "d_tx_h")]
    tx_h: f64,
    #[serde(default = "d_rx_h")]
    rx_h: f64,
    #[serde(default = "d_radius")]
    radius_km: f64,
    #[serde(default = "d_budget")]
    budget_db: f32,
    /// Sweep the whole radius in one band, without the inner bands a map
    /// paints while it waits. Every band is a complete sweep of its own
    /// radius, computed afresh, so the last one is the same raster either
    /// way: a caller that keeps only that one, as sim-mesh's coverage does,
    /// asks this and is spared the rest of the ladder.
    #[serde(default)]
    whole: bool,
    /// `/loss/start` only: the caller's name for this sweep, with the view
    /// and cells (`minx`…`h`) it will ask `/loss.bin` for once the sweep is
    /// whole. sim-mesh's front names a node's sweep by its coverage key, and
    /// `/coverage/bands.bin` then shows the node's coverage while the ladder
    /// grows (`SweepTag`).
    key: Option<String>,
    /// Radials to sweep. Omit for the resolution-matched count.
    ///
    /// Exposed because the right value is a MEASUREMENT, not a constant: the
    /// count needed to resolve a street is a function of range and cell size,
    /// and the cost of it is a function of the machine. Neither was knowable
    /// from inside this file.
    az: Option<usize>,
    /// The VIEW to cut the raster to, and how many cells across to send. All
    /// six together or none: without them the whole raster is sent, which is
    /// what scripted callers and the sweep's own tests need.
    ///
    /// Only `/loss.bin` reads these. `/loss/start` shares the type — one
    /// query struct is what stops a misspelled `tx_h` sweeping the city at
    /// the default height — and `CoverageKey` is built from named fields, so
    /// panning cannot invalidate a sweep.
    minx: Option<f64>,
    miny: Option<f64>,
    maxx: Option<f64>,
    maxy: Option<f64>,
    w: Option<u32>,
    h: Option<u32>,
}

/// Replace the blended clutter with real building heights where a cell is
/// solidly built.
///
/// `ClutterHeight` mixes two independent facts — how tall the buildings are
/// and how much of the cell they cover — and scales the height DOWN when the
/// coverage is small. That is the right input for an area sweep treating
/// every cell as a receiver, and the wrong one for a path crossing a
/// building: a 22 m Vorderhaus in a 10%-built cell arrives as 1.5 m.
///
/// Only cells at or above `SOLID_FRACTION` are replaced. Below it the cell
/// genuinely is mostly not-building, and a path across it probably misses the
/// structure — asserting the full roof height there would trade one bias for
/// a larger one in the other direction. Sub-cell geometry cannot resolve that
/// question, and pretending otherwise would be the same mistake the blend
/// makes.
///
/// Returns the clutter unchanged on any pack without the two layers.
const SOLID_FRACTION: f32 = 0.6;

fn merge_building_tops(
    st: &AppState,
    clutter: Option<planner_terrain::Grid>,
    lo: Xy,
    hi: Xy,
) -> Option<planner_terrain::Grid> {
    let mut c = clutter?;
    let (Some(bt), Some(bf)) = (st.layers.building_top.as_ref(), st.layers.built_fraction.as_ref())
    else {
        return Some(c);
    };
    let top = bt.window(lo, hi).ok()?;
    let frac = bf.window(lo, hi).ok()?;
    if top.data.len() != c.data.len() || frac.data.len() != c.data.len() {
        // Windows must line up cell for cell; if they do not, something about
        // the pack's grids disagrees and silently pairing them by index would
        // put one layer's buildings on another layer's ground.
        return Some(c);
    }
    let mut raised = 0usize;
    for i in 0..c.data.len() {
        if frac.data[i] >= SOLID_FRACTION && top.data[i] > c.data[i] {
            c.data[i] = top.data[i];
            raised += 1;
        }
    }
    if raised > 0 {
        eprintln!(
            "sweep: {raised} of {} cell(s) took a real building height instead of the blend",
            c.data.len()
        );
    }
    Some(c)
}

/// Until the building index has landed or is known to be absent.
///
/// A sweep reads the index once, for the terminal surroundings at the
/// transmitter, and one begun while it loads read `Loading` and ran without
/// them: measured on a 0.5 km sweep, 19,548 of 40,000 cells came out 27 dB
/// lower to 8 dB higher in loss than the same sweep a second later, and
/// nothing in the raster says which it was, so a caller that caches it kept
/// the wrong one. A pair table waits for the index for the same reason (see
/// `profile_evidence.buildings_index`); a sweep has no evidence to carry, so
/// it waits here. A superseded sweep stops waiting.
fn wait_for_index(
    buildings: &std::sync::RwLock<BuildingsIndexState>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    while matches!(*buildings.read().expect("buildings lock"), BuildingsIndexState::Loading) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("superseded by a newer request".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

/// One complete sweep out to `radius_m`. Blocking; call from a worker.
///
/// Split out of the request handler so the same code can serve one shot or a
/// ladder of expanding bands. Each call is a FULL answer for its radius at
/// the geometric azimuth count for that radius — never a coarse preview.
fn run_sweep(
    st: &AppState,
    q: &LossQuery,
    radius_m: f64,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<planner_terrain::Grid, String> {
    wait_for_index(&st.buildings, &cancel)?;
    let tx = match (q.x, q.y) {
        (Some(x), Some(y)) => Xy { x, y },
        _ => st.to_xy(q.lat, q.lon),
    };
    let (_, tx_lat) = st.to_lonlat(tx);
    let lo = st.clamp(Xy { x: tx.x - radius_m, y: tx.y - radius_m });
    let hi = st.clamp(Xy { x: tx.x + radius_m, y: tx.y + radius_m });
    // Without the layers' locks: a 20 km sweep reads a 40 km box, and held
    // through it the lock kept every tile of the map waiting for the read.
    let terrain = st.layers.terrain.window(lo, hi).map_err(|e| e.to_string())?;
    let clutter = st.layers.clutter.as_ref().and_then(|l| l.window(lo, hi).ok());
    // The obstacle the SWEEP gets, which until now was the blended clutter
    // raster alone — the layer whose own documentation calls it a cell mean
    // and the wrong input for one specific path. /link.json had been using
    // real LoD2 heights for a while, so the map and the point-to-point tool
    // were computing different physics on the same pack and the map was the
    // one using the average.
    //
    // Where a cell is solidly built, the building's own representative height
    // replaces the blend. That matters most exactly where the blend is worst:
    // clutter scales DOWN by the built fraction below 0.15, so a real 22 m
    // Vorderhaus in a sparsely-built cell reached the sweep as about 1.5 m.
    let mut clutter = merge_building_tops(&st, clutter, lo, hi);
    let mut link = planner_core::model::LinkParams::eu868_defaults();
    link.tx_h_agl_m = q.tx_h.clamp(1.0, 300.0);
    link.rx_h_agl_m = q.rx_h.clamp(1.0, 100.0);
    link.delta_n = st.manifest.region.delta_n;
    link.n0 = st.manifest.region.n0;
    link.path_center_lat_deg = tx_lat;
    let cell_m = terrain_res_hint(st).max(1.0);
    let az = sweep_azimuths(radius_m, cell_m, q.az);

    // ---- the terminal surroundings P.1812 does not model -------------------
    //
    // This is what makes the MAP show a directional shadow. The transmitter's
    // own roof edge is 8 m away on one bearing and 70 m on another, so the
    // correction runs 16 dB one way and 8 dB the other from the same balcony.
    // A nominal street width would give one number for the whole disc, which
    // cannot draw a shadow at all.
    //
    // Sampled at a bounded number of bearings and held, rather than walked
    // per azimuth: the sweep runs up to 25 133 radials and the shadow of a
    // single building does not have that much angular detail. 720 samples is
    // half a degree, finer than any roof outline resolves.
    let ground_masl = terrain.sample_bilinear(tx).map(|v| v as f64);
    let freq_ghz = link.freq_mhz / 1000.0;
    let tx_h = link.tx_h_agl_m;
    // Per bearing, the correction AND the height P.1812 is run at, both from
    // the one measurement of the clutter on that bearing, exactly as
    // /link.json composes them: raise to R, run the model, add A_h. The sweep
    // once added A_h to a run from the antenna itself, which charges the same
    // obstruction twice; for an antenna under a tall roof of its own that
    // buried every path, while /link.json gave the same antenna usable links.
    type TxTables = (
        Option<std::sync::Arc<dyn Fn(f64) -> f32 + Send + Sync>>,
        Option<std::sync::Arc<dyn Fn(f64) -> f64 + Send + Sync>>,
    );
    let (tx_terminal_db, tx_model_h_m): TxTables = {
        let guard = st.buildings.read().expect("buildings lock");
        let indoor = match (guard.get(), ground_masl) {
            (Some(ix), Some(g)) => indoor_terminal(ix, tx.x, tx.y, g + tx_h, freq_ghz),
            _ => None,
        };
        match (guard.get(), ground_masl) {
            // Inside a building: its own building is not an obstacle on the
            // paths out of it, the antenna stays where it is, and every
            // bearing pays the same entry loss (see `Indoor`), as /link.json
            // charges it.
            (Some(ix), Some(_)) if indoor.is_some() => {
                let d = indoor.unwrap();
                if let Some(c) = clutter.as_mut() {
                    for row in 0..c.height {
                        for col in 0..c.width {
                            let x = c.origin.x + col as f64 * c.dx_m;
                            let y = c.origin.y + row as f64 * c.dy_m;
                            if ix.holds(d.building, x, y) {
                                c.data[row * c.width + col] = 0.0;
                            }
                        }
                    }
                }
                let entry = d.entry_db as f32;
                (
                    Some(std::sync::Arc::new(move |_: f64| entry)
                        as std::sync::Arc<dyn Fn(f64) -> f32 + Send + Sync>),
                    Some(std::sync::Arc::new(move |_: f64| tx_h)
                        as std::sync::Arc<dyn Fn(f64) -> f64 + Send + Sync>),
                )
            }
            (Some(ix), Some(g)) => {
                const N: usize = 720;
                let at = |ang: f64| -> (f32, f64) {
                    match ix.clutter_along(tx.x, tx.y, g, g + tx_h, ang, 120.0, 2.0) {
                        Some((r, ws)) => (
                            planner_propag::p2108::height_gain_correction(
                                freq_ghz,
                                tx_h,
                                r,
                                ws,
                                planner_propag::p2108::TerminalClutter::Obstructed,
                            )
                            .unwrap_or(0.0) as f32,
                            planner_propag::p2108::model_height_m(tx_h, r),
                        ),
                        None => (0.0, tx_h),
                    }
                };
                let (loss, height): (Vec<f32>, Vec<f64>) = (0..N)
                    .map(|i| at(std::f64::consts::TAU * i as f64 / N as f64))
                    .unzip();
                let index = |ang: f64| {
                    let t = ang.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU;
                    ((t * N as f64) as usize).min(N - 1)
                };
                (
                    Some(std::sync::Arc::new(move |ang: f64| loss[index(ang)])
                        as std::sync::Arc<dyn Fn(f64) -> f32 + Send + Sync>),
                    Some(std::sync::Arc::new(move |ang: f64| height[index(ang)])
                        as std::sync::Arc<dyn Fn(f64) -> f64 + Send + Sync>),
                )
            }
            _ => (None, None),
        }
    };
    // The receiver end uses P.2108's nominal 27 m street width. Unlike the
    // transmitter there is a receiver in every cell, and a ray walk each would
    // cost more than the propagation; R still comes from the pack's own
    // clutter at that cell, so only the distance is nominal.
    let rx_terminal = Some(planner_coverage::RxTerminal {
        freq_ghz,
        h_agl_m: link.rx_h_agl_m,
        ws_m: 27.0,
    });

    st.sweep_pool
        .install(|| {
            planner_coverage::coverage(
                &terrain,
                clutter.as_ref(),
                &planner_coverage::CoverageParams {
                    tx,
                    radius_m,
                    link,
                    max_azimuths: Some(az),
                    cancel: Some(cancel),
                    tx_terminal_db,
                    tx_model_h_m,
                    rx_terminal,
                    stop_above_db: Some(q.budget_db + 18.0),
                    stop_after_m: planner_coverage::DEFAULT_STOP_AFTER_M,
                    max_profile_points: planner_coverage::MAX_PROFILE_POINTS,
                    table_budget_bytes: planner_coverage::DEFAULT_TABLE_BUDGET_BYTES,
                },
            )
        })
        .map(|c| c.loss)
        .map_err(|e| e.to_string())
}

/// The latest finished band, whatever radius it reached.
///
/// Never computes. A sweep is started by `/loss/start` and this serves what
/// has landed, so the map can paint a finished inner band while the outer
/// ones are still running.
/// A loss as `/loss.bin` sends it: decibels ×100 as u16, 0–655 dB covering
/// everything, and NaN as the maximum.
fn loss_centibels(v: f32) -> u16 {
    if v.is_finite() {
        (v * 100.0).clamp(0.0, 65534.0) as u16
    } else {
        u16::MAX
    }
}

async fn loss_bin(State(st): State<Arc<AppState>>, Query(q): Query<LossQuery>) -> Response {
    let tx = match (q.x, q.y) {
        (Some(x), Some(y)) => Xy { x, y },
        _ => st.to_xy(q.lat, q.lon),
    };
    let (tx_lon, _tx_lat) = st.to_lonlat(tx);
    let cache = st.coverage_cache.lock().await;
    let Some(cached) = cache.as_ref() else {
        return (StatusCode::NO_CONTENT, "no sweep has finished a band yet").into_response();
    };
    let ms = cached.compute_ms;
    let band_km = cached.key.radius_km;
    let (_, tx_lat) = st.to_lonlat(tx);
    let _ = tx_lat;
    let g: &planner_terrain::Grid = &cached.loss;
    // Cut to the view when the page asked for one. See `window_raster` for the
    // measured reason: the whole raster is 128 MB at 20 km and 288 MB at 30 km,
    // per band, all of it sampled down to one value per screen pixel on
    // arrival.
    let (w, h, res_x, res_y, origin, data) =
        match requested_view(q.minx, q.miny, q.maxx, q.maxy) {
            Some(view) => {
                // `window_raster` is a rayon parallel loop. Run it under
                // block_in_place, as tile_bin does its reads: a rayon wait on
                // a tokio worker thread parks that worker for the duration,
                // and with a tile update issuing four requests at once that is
                // a third of the runtime gone for a resample. tile_bin already
                // observes this rule; the two overlay endpoints did not.
                let (w, h, rx, ry, o, d) = tokio::task::block_in_place(|| {
                    window_raster(
                        g,
                        &view,
                        q.w.unwrap_or(1024),
                        q.h.unwrap_or(1024),
                        Resample::Bilinear,
                    )
                });
                (w, h, rx, ry, o, std::borrow::Cow::Owned(d))
            }
            None => (
                g.width as u32,
                g.height as u32,
                g.dx_m,
                g.dy_m.abs(),
                g.origin,
                std::borrow::Cow::Borrowed(&g.data[..]),
            ),
        };
    let mut out = Vec::with_capacity(44 + data.len() * 2);
    out.extend_from_slice(b"PLS2");
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&origin.x.to_le_bytes());
    out.extend_from_slice(&origin.y.to_le_bytes());
    out.extend_from_slice(&res_x.to_le_bytes());
    out.extend_from_slice(&res_y.to_le_bytes());
    for &v in data.iter() {
        out.extend_from_slice(&loss_centibels(v).to_le_bytes());
    }
    let mut resp = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        out,
    )
        .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&ms.to_string()) {
        resp.headers_mut().insert("x-compute-ms", v);
    }
    // The true geographic position of the sweep centre, projected here rather
    // than guessed in the browser.
    let (lon, lat) = st.to_lonlat(tx);
    let _ = tx_lon;
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("{lon:.6},{lat:.6}")) {
        resp.headers_mut().insert("x-lonlat", v);
    }
    // Which BAND this is. The map must be able to say "finished out to 4 km,
    // still growing" rather than presenting a partial sweep as the whole
    // answer -- the inner band is correct, but it is not yet complete.
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("{band_km:.3}")) {
        resp.headers_mut().insert("x-band-km", v);
    }
    resp
}

/// Start a sweep that grows outward, or report the one already running.
async fn loss_start(State(st): State<Arc<AppState>>, Query(q): Query<LossQuery>) -> Response {
    let radius_m = q.radius_km.clamp(0.5, MAX_COVERAGE_RADIUS_KM) * 1000.0;
    let tx = match (q.x, q.y) {
        (Some(x), Some(y)) => Xy { x, y },
        _ => st.to_xy(q.lat, q.lon),
    };
    let (lon, lat) = st.to_lonlat(tx);
    let cell_m = terrain_res_hint(&st).max(1.0);
    let key = CoverageKey {
        lat,
        lon,
        tx_h: q.tx_h,
        rx_h: q.rx_h,
        radius_km: q.radius_km,
        budget_db: q.budget_db,
        azimuths: sweep_azimuths(radius_m, cell_m, q.az),
    };
    {
        let g = st.sweep.lock().await;
        if let ProgressiveSweep::Running { key: running, cancel, .. } = &*g {
            if *running == key {
                // Same request. Join it rather than starting a second sweep
                // that would compute the identical answer and fight the first
                // one for memory.
                let c = st.coverage_cache.lock().await;
                return axum::Json(sweep_status(&g, &c)).into_response();
            }
            // Different request: STOP the old sweep rather than queue behind
            // it. Without this a superseding request failed outright with
            // "another sweep is still running", which is what every move of
            // the receiver-height scrubber did -- the control looked dead
            // while the server was busy computing an answer nobody wanted any
            // more.
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    // Publish Running BEFORE returning, and drop the old transmitter's raster
    // in the same breath.
    //
    // Doing this inside the spawned task left a window where /loss/start had
    // returned but the state still read `idle`, and the page polls status in
    // a loop that exits as soon as the state is not `running`: it exited
    // immediately, painted whatever was in the cache, and stopped. Moving a
    // transmitter therefore drew nothing at all. The state must be true from
    // the instant the caller is told the sweep began.
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let started = std::time::Instant::now();
    let bands: &'static [f64] = if q.whole { &[1.0] } else { SWEEP_BANDS };
    let tag = match (&q.key, requested_view(q.minx, q.miny, q.maxx, q.maxy), q.w, q.h) {
        (Some(k), Some(view), Some(w), Some(h)) if valid_key(k) => {
            Some(Arc::new(SweepTag { key: k.clone(), view, w, h }))
        }
        _ => None,
    };
    {
        let mut g = st.sweep.lock().await;
        *g = ProgressiveSweep::Running {
            key,
            bands: bands.len(),
            done: 0,
            band_radius_m: 0.0,
            started: Some(started),
            cancel: cancel.clone(),
        };
        // A raster from the PREVIOUS transmitter must not stay visible under
        // the new marker while the new sweep runs.
        *st.coverage_cache.lock().await = None;
    }
    // A different request supersedes: only one sweep fits in memory, so wait
    // for the slot rather than refusing. The permit is held for the whole
    // ladder and released when the task ends.
    let st2 = st.clone();
    let q2 = q.clone();
    tokio::task::spawn_blocking(move || {
        // WAIT for the slot; do not fail against it. The predecessor has been
        // told to stop and checks its flag once per azimuth, so this is a
        // short wait, and a superseding request must never be refused just
        // because the thing it supersedes has not noticed yet. `acquire` is
        // async and this is a blocking task, so spin rather than pull in a
        // runtime handle.
        let _permit = loop {
            if let Ok(p) = st2.sweep_slots.try_acquire() {
                break p;
            }
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return; // superseded again before we ever started
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        for (i, frac) in bands.iter().enumerate() {
            let band_m = (radius_m * frac).max(cell_m * 8.0);
            let t0 = std::time::Instant::now();
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return; // superseded between bands
            }
            match run_sweep(&st2, &q2, band_m, cancel.clone()) {
                Ok(loss) => {
                    let mut band_key = key;
                    band_key.radius_km = band_m / 1000.0;
                    *st2.coverage_cache.blocking_lock() = Some(CachedCoverage {
                        key: band_key,
                        loss: Arc::new(loss),
                        compute_ms: t0.elapsed().as_millis(),
                        tag: tag.clone(),
                    });
                    // Same guard: a band that finished just as this sweep
                    // was superseded must not publish itself over the new
                    // one, or the page would show one transmitter's progress
                    // for another transmitter's sweep.
                    let mut g = st2.sweep.blocking_lock();
                    if matches!(&*g, ProgressiveSweep::Running { key: k, .. } if *k == key) {
                        *g = ProgressiveSweep::Running {
                            key,
                            bands: bands.len(),
                            done: i + 1,
                            band_radius_m: band_m,
                            started: Some(started),
                            cancel: cancel.clone(),
                        };
                    } else {
                        return;
                    }
                }
                // A cancelled sweep is not a failure and must not be reported
                // as one: the request that replaced it is about to set its
                // own state, and an error here would flash in the UI as
                // though something had gone wrong.
                Err(e) if e.contains("superseded") => return,
                Err(e) => {
                    *st2.sweep.blocking_lock() = ProgressiveSweep::Failed(e);
                    return;
                }
            }
            // The last band already covers the request; stop before spending
            // another quarter of the budget on a duplicate.
            if band_m >= radius_m - 1.0 {
                break;
            }
        }
        // Only the CURRENT sweep may declare the run finished. A cancelled
        // predecessor reaching this line would set Idle over its successor's
        // Running state, and the page would stop polling a sweep that is
        // still going.
        {
            let mut g = st2.sweep.blocking_lock();
            if matches!(&*g, ProgressiveSweep::Running { key: k, .. } if *k == key) {
                *g = ProgressiveSweep::Idle;
            }
        }
    });
    axum::Json(serde_json::json!({
        "state": "running", "bands": bands.len(), "done": 0
    }))
    .into_response()
}

async fn loss_status(State(st): State<Arc<AppState>>) -> Response {
    let g = st.sweep.lock().await;
    let c = st.coverage_cache.lock().await;
    axum::Json(sweep_status(&g, &c)).into_response()
}

fn sweep_status(
    s: &ProgressiveSweep,
    cache: &Option<CachedCoverage>,
) -> serde_json::Value {
    let band_km = cache.as_ref().map(|c| c.key.radius_km).unwrap_or(0.0);
    match s {
        ProgressiveSweep::Idle => serde_json::json!({
            "state": "idle", "band_km": band_km, "bands": SWEEP_BANDS.len(),
            "done": if band_km > 0.0 { SWEEP_BANDS.len() } else { 0 }
        }),
        ProgressiveSweep::Running { bands, done, band_radius_m, started, .. } => serde_json::json!({
            "state": "running",
            "done": done,
            "bands": bands,
            "band_km": band_radius_m / 1000.0,
            "ready_km": band_km,
            "elapsed_s": started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
        }),
        ProgressiveSweep::Failed(e) => serde_json::json!({ "state": "failed", "error": e }),
    }
}

/// A number the page sends as JavaScript writes it, in a string, read
/// exactly. serde_json's own reading of a 17-digit number can land an ulp
/// away (see `LinksRequest`), and a view or a node an ulp away can be
/// another cell.
fn exact<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let text = String::deserialize(d)?;
    text.parse().map_err(serde::de::Error::custom)
}

fn exact_opt<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    match Option::<String>::deserialize(d)? {
        None => Ok(None),
        Some(text) => text.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// `POST /coverage/bands.bin`'s body: a view, the bands, and the nodes whose
/// coverage it shows. Every number is a string (`exact`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BandsRequest {
    /// The geodata whose rasters these are: the front caches them as
    /// `<coverage-dir>/<geodata>/<key>.bin`.
    geodata: String,
    /// The view: its centre in the pack's metres, metres per CSS pixel, its
    /// size in CSS pixels, and the side of a cell in them.
    #[serde(deserialize_with = "exact")]
    cx: f64,
    #[serde(deserialize_with = "exact")]
    cy: f64,
    #[serde(deserialize_with = "exact")]
    mpp: f64,
    #[serde(deserialize_with = "exact")]
    w: f64,
    #[serde(deserialize_with = "exact")]
    h: f64,
    #[serde(deserialize_with = "exact")]
    px: f64,
    /// Each band's least margin in dB, the best band first.
    bands: Vec<String>,
    /// The receiver's height above the ground, the one the rasters were
    /// swept to.
    #[serde(deserialize_with = "exact")]
    rx_h: f64,
    nodes: Vec<BandsNode>,
}

/// One node: its raster's key, where it stands, what it has to spend and
/// its antenna.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BandsNode {
    key: String,
    #[serde(deserialize_with = "exact")]
    x: f64,
    #[serde(deserialize_with = "exact")]
    y: f64,
    /// The antenna's height above the ground under it.
    #[serde(deserialize_with = "exact")]
    height_m: f64,
    /// Transmit power plus the receiver's gain, less the decoding threshold:
    /// the margin at a point is this, plus the antenna's gain toward it,
    /// less the loss.
    #[serde(deserialize_with = "exact")]
    budget_db: f64,
    /// Its pattern, `None` for an antenna the catalogue does not have.
    antenna: Option<BandsAntenna>,
}

/// An antenna's pattern, as sim-mesh's catalogue gives it (testbed/antennas.py,
/// the page's lib/antennas.ts), and its aim.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BandsAntenna {
    directional: bool,
    #[serde(deserialize_with = "exact")]
    peak_dbi: f64,
    #[serde(deserialize_with = "exact")]
    vbw_deg: f64,
    #[serde(deserialize_with = "exact")]
    tilt_deg: f64,
    #[serde(default, deserialize_with = "exact_opt")]
    hbw_deg: Option<f64>,
    #[serde(deserialize_with = "exact")]
    floor_db: f64,
    #[serde(deserialize_with = "exact")]
    azimuth_deg: f64,
    #[serde(deserialize_with = "exact")]
    elevation_deg: f64,
}

/// What `/coverage/bands.bin` answers for a cell where no band is reached.
const NO_BAND: u8 = 255;
/// A raster cell nothing was evaluated for.
const NEVER_LOSS: u16 = u16::MAX;
/// The cells of one view, at most.
const MAX_BAND_CELLS: f64 = 4_000_000.0;
/// What the read rasters may hold, the terrain under them included. About
/// what the page itself held for 20 nodes on a 2048-cell raster each, when
/// it combined them.
const COVERAGE_KEPT_BYTES: usize = 512 << 20;

impl BandsRequest {
    fn check(&self) -> Result<Vec<f64>, String> {
        if !valid_name(&self.geodata) {
            return Err(format!("{:?} is not a geodata name", self.geodata));
        }
        if let Some(n) = self.nodes.iter().find(|n| !valid_key(&n.key)) {
            return Err(format!("{:?} is not a coverage key", n.key));
        }
        let good = |v: f64| v.is_finite() && v > 0.0;
        if !(good(self.mpp) && good(self.w) && good(self.h) && good(self.px)) || !self.cx.is_finite()
            || !self.cy.is_finite()
        {
            return Err("the view needs a finite centre and positive scale, size and cell".into());
        }
        if (self.w / self.px).ceil() * (self.h / self.px).ceil() > MAX_BAND_CELLS {
            return Err("the view has more cells than one reply holds".into());
        }
        let bands: Vec<f64> =
            self.bands.iter().map(|b| b.parse::<f64>()).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        if bands.is_empty() || bands.len() >= usize::from(NO_BAND) {
            return Err("name between one and 254 bands".into());
        }
        Ok(bands)
    }
}

/// A geodata name as sim-mesh's store has them (store.NAME_RE).
fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    let inner = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-';
    let end = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty() && b.len() <= 32 && end(&b[0]) && end(&b[b.len() - 1]) && b.iter().all(inner)
}

/// A coverage key as the front makes them (coverage.key): 16 hex digits.
fn valid_key(key: &str) -> bool {
    key.len() == 16 && key.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// `Math.round`: the nearest whole number, a half rounding up, as the page
/// rounds. `f64::round` takes a half away from zero instead.
fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// `Math.hypot` of two, as V8 computes it: each scaled by the larger, the
/// squares summed with Kahan's compensation, the root scaled back.
fn js_hypot(a: f64, b: f64) -> f64 {
    let (a, b) = (a.abs(), b.abs());
    if a.is_infinite() || b.is_infinite() {
        return f64::INFINITY;
    }
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    let max = a.max(b);
    if max == 0.0 {
        return 0.0;
    }
    let (mut sum, mut compensation) = (0.0f64, 0.0f64);
    for v in [a, b] {
        let n = v / max;
        let summand = n * n - compensation;
        let preliminary = sum + summand;
        compensation = (preliminary - sum) - summand;
        sum = preliminary;
    }
    sum.sqrt() * max
}

/// `Math.min` of two: NaN when either is.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if b < a {
        b
    } else {
        a
    }
}

impl BandsAntenna {
    /// Gain in dBi toward azimuth `az` (clockwise from grid north) and
    /// elevation `el`, degrees: the page's `gain` (lib/antennas.ts). The loss
    /// below the peak is 12·((el − tilt)/vbw)², plus for a directional
    /// antenna 12·(az/hbw)², together never more than the floor.
    fn gain(&self, az: f64, el: f64) -> f64 {
        let wrap180 = |deg: f64| ((deg + 180.0) % 360.0 + 360.0) % 360.0 - 180.0;
        let (mut tilt, mut off) = (self.tilt_deg, 0.0);
        if self.directional {
            tilt += self.elevation_deg;
            off = wrap180(az - self.azimuth_deg);
        }
        let v = (el - tilt) / self.vbw_deg;
        let mut down = 12.0 * (v * v);
        if let Some(hbw) = self.hbw_deg.filter(|&h| self.directional && h != 0.0 && !h.is_nan()) {
            let o = off / hbw;
            down += 12.0 * (o * o);
        }
        self.peak_dbi - js_min(down, self.floor_db)
    }
}

/// Azimuth and elevation in degrees of the line from one antenna tip to
/// another, the far one dropped by the earth's curvature at k = 4/3: the
/// page's `direction` (lib/antennas.ts).
fn tip_direction(ax: f64, ay: f64, a_top: f64, bx: f64, by: f64, b_top: f64) -> (f64, f64) {
    const EARTH_RADIUS_M: f64 = 6371008.8;
    const K_FACTOR: f64 = 4.0 / 3.0;
    let (dx, dy) = (bx - ax, by - ay);
    let d = js_hypot(dx, dy);
    let az = ((dx.atan2(dy) * 180.0 / std::f64::consts::PI) % 360.0 + 360.0) % 360.0;
    let drop = d * d / (2.0 * K_FACTOR * EARTH_RADIUS_M);
    let el = (b_top - a_top - drop).atan2(d.max(1e-6)) * 180.0 / std::f64::consts::PI;
    (az, el)
}

/// A tile's terrain as the page reads it: rows south from the top-left
/// cell's centre, a cell `dm * 0.1` metres held as an f32.
struct TerrainTile {
    w: usize,
    h: usize,
    ox: f64,
    oy: f64,
    rx: f64,
    ry: f64,
    dm: Vec<i16>,
}

impl TerrainTile {
    /// The terrain `tile.bin?…&terrain_only=1` sends for `view` at `w`×`h`
    /// cells, as that route computes it. Blocking.
    fn read(st: &AppState, view: &ViewRect, w: u32, h: u32) -> Result<Self, String> {
        let res_hint = terrain_res_hint(st);
        let (tw, th) = tile_dims(view, w, h, res_hint, res_hint);
        let lo = st.clamp(Xy { x: view.min_x, y: view.min_y });
        let hi = st.clamp(Xy { x: view.max_x, y: view.max_y });
        let g = st
            .layers
            .terrain
            .window_max(lo, hi, (tw as usize) * 2, (th as usize) * 2)
            .map_err(|e| e.to_string())?;
        let bytes = tile_block(&g, view, tw, th, 2, &terrain_decimetres);
        let (rx, ry) = (view.width_m() / tw as f64, view.height_m() / th as f64);
        Ok(TerrainTile {
            w: tw as usize,
            h: th as usize,
            ox: view.min_x + 0.5 * rx,
            oy: view.max_y - 0.5 * ry,
            rx,
            ry,
            dm: bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect(),
        })
    }

    /// The nearest cell's terrain at (x, y), `None` off the tile or where it
    /// has none: the page's `terrainAt`.
    fn at(&self, x: f64, y: f64) -> Option<f64> {
        let col = js_round((x - self.ox) / self.rx.abs());
        let row = js_round((self.oy - y) / self.ry.abs());
        if col < 0.0 || row < 0.0 || col >= self.w as f64 || row >= self.h as f64 {
            return None;
        }
        let dm = self.dm[row as usize * self.w + col as usize];
        (dm != i16::MIN).then(|| (f64::from(dm) * 0.1) as f32 as f64)
    }
}

/// A node's coverage raster as the front caches it (sim-mesh's
/// testbed/coverage.py): "PLS2" | u32 w | u32 h | f64 ox | f64 oy | f64 rx |
/// f64 ry | u16 loss·100 [w·h], row-major from the north-west, (ox, oy) the
/// first cell's centre, 65535 where nothing was evaluated. With it the
/// terrain under it, on its own grid: what the page fetched beside each
/// raster for each cell's elevation, `None` where that could not be read.
struct CoverageRaster {
    w: usize,
    h: usize,
    ox: f64,
    oy: f64,
    rx: f64,
    ry: f64,
    loss: Vec<u16>,
    terrain: Option<TerrainTile>,
}

impl CoverageRaster {
    fn parse(data: &[u8]) -> Option<(usize, usize, [f64; 4], Vec<u16>)> {
        if data.len() < 44 || &data[..4] != b"PLS2" {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        let f64_at = |i: usize| f64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        let (w, h) = (u32_at(4), u32_at(8));
        let cells = data.get(44..44 + w * h * 2)?;
        let loss = cells.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        Some((w, h, [f64_at(12), f64_at(20), f64_at(28), f64_at(36)], loss))
    }

    fn bytes(&self) -> usize {
        self.loss.len() * 2 + self.terrain.as_ref().map_or(0, |t| t.dm.len() * 2)
    }
}

/// The raster of `key` on `geodata` with its terrain: from those read
/// before, else from the front's cache, else, while this sidecar's sweep for
/// that key grows, from the band it finished last; `None` when there is none
/// of these (yet). Blocking.
fn coverage_raster(
    st: &AppState,
    dir: &Path,
    geodata: &str,
    key: &str,
) -> Result<Option<Arc<CoverageRaster>>, String> {
    let path = dir.join(geodata).join(format!("{key}.bin"));
    let held = |name: &str| {
        let mut held = st.coverage_rasters.lock().expect("coverage rasters lock");
        let i = held.iter().position(|(n, _)| n == name)?;
        let hit = held.remove(i);
        let raster = Arc::clone(&hit.1);
        held.push(hit);
        Some(raster)
    };
    let name = path.display().to_string();
    if let Some(raster) = held(&name) {
        return Ok(Some(raster));
    }
    let (name, (w, h, [ox, oy, rx, ry], loss)) = match std::fs::read(&path) {
        Ok(data) => match CoverageRaster::parse(&data) {
            Some(parsed) => (name, parsed),
            None => return Err(format!("{name} is not a PLS2 raster")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Not cached yet: the band of its sweep finished last, when this
            // sidecar is sweeping it, as `/loss.bin` will cut and send it.
            let Some(band) = st.coverage_cache.blocking_lock().as_ref().and_then(|c| {
                let tag = c.tag.as_ref().filter(|t| t.key == key)?;
                Some((Arc::clone(&c.loss), Arc::clone(tag), c.key.radius_km))
            }) else {
                return Ok(None);
            };
            let (grid, tag, band_km) = band;
            let name = format!("{name}@{band_km}");
            if let Some(raster) = held(&name) {
                return Ok(Some(raster));
            }
            let (w, h, rx, ry, o, data) =
                window_raster(&*grid, &tag.view, tag.w, tag.h, Resample::Bilinear);
            let loss = data.into_iter().map(loss_centibels).collect();
            (name, (w as usize, h as usize, [o.x, o.y, rx, ry], loss))
        }
        Err(e) => return Err(format!("{name}: {e}")),
    };
    // The box the page asked `tile.bin` for: the raster's cells, out to their edges.
    let view = ViewRect {
        min_x: ox - rx / 2.0,
        max_x: ox + (w as f64 - 0.5) * rx,
        max_y: oy + ry / 2.0,
        min_y: oy - (h as f64 - 0.5) * ry,
    };
    let terrain = TerrainTile::read(st, &view, w as u32, h as u32).ok();
    let raster = Arc::new(CoverageRaster { w, h, ox, oy, rx, ry, loss, terrain });
    let mut held = st.coverage_rasters.lock().expect("coverage rasters lock");
    held.retain(|(n, _)| *n != name);
    held.push((name, Arc::clone(&raster)));
    let mut total: usize = held.iter().map(|(_, r)| r.bytes()).sum();
    while total > COVERAGE_KEPT_BYTES && held.len() > 1 {
        total -= held.remove(0).1.bytes();
    }
    Ok(Some(raster))
}

/// One node's margin over its raster's cells, read as a raster: a cell's is
/// the node's budget plus its antenna's gain toward the cell (from the tip
/// to a receiver `rx_h` above the terrain there) less the cell's loss; NaN
/// where the raster evaluated nothing. Read NEAREST, as the page read it: a
/// point's margin is its cell's, the gain worked out for the cell's centre.
struct NodeMargins<'a> {
    raster: &'a CoverageRaster,
    node: &'a BandsNode,
    rx_h: f64,
    /// The ground under the node, from the raster's terrain (0 where there
    /// is none), and its antenna's tip above sea level.
    ground: f64,
    top: f64,
}

impl<'a> NodeMargins<'a> {
    fn new(raster: &'a CoverageRaster, node: &'a BandsNode, rx_h: f64) -> Self {
        let ground = raster.terrain.as_ref().and_then(|t| t.at(node.x, node.y)).unwrap_or(0.0);
        NodeMargins { raster, node, rx_h, ground, top: ground + node.height_m }
    }
}

impl ViewSource for NodeMargins<'_> {
    fn origin(&self) -> Xy {
        Xy { x: self.raster.ox, y: self.raster.oy }
    }
    fn dx_m(&self) -> f64 {
        self.raster.rx
    }
    fn dy_m(&self) -> f64 {
        -self.raster.ry
    }
    fn width(&self) -> usize {
        self.raster.w
    }
    fn height(&self) -> usize {
        self.raster.h
    }
    /// The nearest cell, found as `Grid::sample_nearest` finds it.
    fn sample(&self, p: Xy, _how: Resample) -> Option<f32> {
        let r = self.raster;
        let fx = (p.x - r.ox) / r.rx;
        let fy = (p.y - r.oy) / -r.ry;
        let eps = 1e-9;
        if fx < -0.5 - eps || fy < -0.5 - eps || fx > r.w as f64 - 0.5 + eps || fy > r.h as f64 - 0.5 + eps
        {
            return None;
        }
        let col = (fx.round() as isize).clamp(0, r.w as isize - 1) as usize;
        let row = (fy.round() as isize).clamp(0, r.h as isize - 1) as usize;
        let v = r.loss[row * r.w + col];
        if v == NEVER_LOSS {
            return Some(f32::NAN);
        }
        let (x, y) = (r.ox + col as f64 * r.rx, r.oy - row as f64 * r.ry);
        let gain = match &self.node.antenna {
            None => 0.0,
            Some(a) => {
                let rx_ground = r.terrain.as_ref().and_then(|t| t.at(x, y)).unwrap_or(self.ground);
                let (az, el) =
                    tip_direction(self.node.x, self.node.y, self.top, x, y, rx_ground + self.rx_h);
                a.gain(az, el)
            }
        };
        Some((self.node.budget_db + gain - f64::from(v) / 100.0) as f32)
    }
}

/// The coverage band of every cell of a view over a set of nodes, each
/// node's margin from its raster in the front's cache.
///
/// The page did this itself: it fetched each node's raster (3.4 MB on the
/// berlin-centre pack) and the terrain under it (as much again), kept both
/// for the page's life, and combined them into one grid of the best margin,
/// an antenna gain worked out per cell, before it could draw anything: 6 s
/// of its main thread for two nodes. Here the rasters stay beside the
/// planner, and a view is a few hundred thousand cells whatever the pack.
///
/// Each node's margins are resampled onto the view's cells as a coverage
/// window is (`window_cells`, read nearest), and a cell's margin is the best
/// of them, held as an f32 as the page's grid held it. Its band is the first
/// whose least margin it reaches; below 0 dB, or where no raster reaches,
/// there is none. The cells are the page's, `px` CSS pixels square from the
/// view's top-left corner, so the page draws exactly the picture it drew.
///
/// Reply: "PCB1" | u32 cols | u32 rows | u8 band[cols·rows], rows from the
/// top, 255 for none. A node whose raster the cache does not have is left
/// out, as the page left it out until its raster came.
async fn coverage_bands(
    State(st): State<Arc<AppState>>,
    axum::Json(req): axum::Json<BandsRequest>,
) -> Response {
    let Some(dir) = st.coverage_dir.clone() else {
        return (StatusCode::NOT_FOUND, "no coverage cache: planner-web runs without --coverage-dir")
            .into_response();
    };
    let bands = match req.check() {
        Ok(bands) => bands,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let Ok(_permit) = st.slots.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "renderer busy").into_response();
    };
    match tokio::task::block_in_place(|| band_grid(&st, &dir, &req, &bands)) {
        Ok(out) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            out,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// `/coverage/bands.bin`'s reply. Blocking.
fn band_grid(st: &AppState, dir: &Path, req: &BandsRequest, bands: &[f64]) -> Result<Vec<u8>, String> {
    let rasters: Vec<Option<Arc<CoverageRaster>>> = req
        .nodes
        .par_iter()
        .map(|n| coverage_raster(st, dir, &req.geodata, &n.key))
        .collect::<Result<_, _>>()?;
    let (cols, rows) = ((req.w / req.px).ceil() as u32, (req.h / req.px).ceil() as u32);
    // The page's cells: their centres `(i + 0.5)·px` CSS pixels from the
    // view's top-left corner.
    let (left, top) = (req.cx - req.w / 2.0 * req.mpp, req.cy + req.h / 2.0 * req.mpp);
    let view = ViewRect {
        min_x: left,
        max_x: left + f64::from(cols) * req.px * req.mpp,
        max_y: top,
        min_y: top - f64::from(rows) * req.px * req.mpp,
    };
    let (w, h) = (cols as usize, rows as usize);
    let mut best = vec![f32::NAN; w * h];
    for (node, raster) in req.nodes.iter().zip(&rasters) {
        let Some(raster) = raster else { continue };
        let margins = NodeMargins::new(raster, node, req.rx_h);
        let (ow, oh, res_x, res_y, origin, data) =
            window_cells(&margins, &view, cols, rows, Resample::Nearest);
        // Where the window, cut to the raster, starts among the view's cells.
        let c0 = ((origin.x - view.min_x) / res_x - 0.5).round() as usize;
        let r0 = ((view.max_y - origin.y) / res_y - 0.5).round() as usize;
        for row in 0..oh as usize {
            let line = &mut best[(r0 + row) * w + c0..(r0 + row) * w + c0 + ow as usize];
            for (b, &m) in line.iter_mut().zip(&data[row * ow as usize..(row + 1) * ow as usize]) {
                if !m.is_nan() && (b.is_nan() || m > *b) {
                    *b = m;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(12 + w * h);
    out.extend_from_slice(b"PCB1");
    out.extend_from_slice(&cols.to_le_bytes());
    out.extend_from_slice(&rows.to_le_bytes());
    out.extend(best.iter().map(|&m| {
        if !(m >= 0.0) {
            return NO_BAND;
        }
        let m = f64::from(m);
        bands.iter().position(|&from| m >= from).unwrap_or(bands.len() - 1) as u8
    }));
    Ok(out)
}

/// Radio presets with the link budget SPELLED OUT.
///
/// The budget slider used to be a bare dB number, which is not interpretable
/// without knowing where it came from. Every preset here reports its own
/// arithmetic — TX power, antenna gain, receiver sensitivity, fade margin —
/// so the threshold on the map is traceable to a radio configuration rather
/// than to a number someone dragged.
async fn presets_json() -> Response {
    use planner_core::preset::{
        eu_erp_ceiling_dbm, max_path_loss_for, sensitivity_dbm, GENERIC_SX1262, RADIO_PRESETS,
    };
    const FADE_MARGIN_DB: f64 = 10.0;
    let dev = &GENERIC_SX1262;
    let out: Vec<serde_json::Value> = RADIO_PRESETS
        .iter()
        .map(|r| {
            let sens = sensitivity_dbm(r.spreading_factor, r.bw_khz);
            let ceiling = eu_erp_ceiling_dbm(r.freq_mhz);
            // The regulatory ladder, as offered per site by the optimiser.
            let powers: Vec<serde_json::Value> = [2.0, 7.0, 14.0, 17.0, 20.0, 22.0]
                .iter()
                .filter(|p| **p <= dev.tx_power_dbm)
                .map(|p| {
                    serde_json::json!({
                        "tx_dbm": p,
                        "max_loss_db": max_path_loss_for(*p, dev.antenna_gain_dbi, r, FADE_MARGIN_DB),
                        "over_erp": *p + dev.antenna_gain_dbi > ceiling,
                    })
                })
                .collect();
            serde_json::json!({
                "id": r.id,
                "freq_mhz": r.freq_mhz,
                "bw_khz": r.bw_khz,
                "sf": r.spreading_factor,
                "sensitivity_dbm": sens,
                "antenna_gain_dbi": dev.antenna_gain_dbi,
                "fade_margin_db": FADE_MARGIN_DB,
                "erp_ceiling_dbm": ceiling,
                "powers": powers,
            })
        })
        .collect();
    axum::Json(serde_json::json!({ "device": dev.id, "radios": out })).into_response()
}

/// `deny_unknown_fields` is load-bearing, not tidiness.
///
/// Every height and budget field carries `#[serde(default)]`, so a caller that
/// misspells one — `ah` instead of `tx_h` — does not get an error. It gets a
/// confident answer computed at the DEFAULT height, and nothing in the reply
/// says which height was used. That is the worst failure mode an instrument
/// can have: a survey probing two antenna heights got byte-identical margins
/// on every path, which reads as "height does not matter here" rather than
/// "your parameter never arrived". Rejecting unknown fields converts a silent
/// wrong number into a 4xx. The browser sends exactly these names.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkQuery {
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    #[serde(default = "d_tx_h")]
    tx_h: f64,
    #[serde(default = "d_rx_h")]
    rx_h: f64,
    /// Explicit budget. Wins over anything derived from the antennas below —
    /// a caller who states a budget has already done this arithmetic.
    budget_db: Option<f32>,
    /// Antenna gain at each end (dBi). The budget is a sum of four terms and
    /// two of them are the antennas:
    ///     budget = tx_power + tx_gain + rx_gain - sensitivity - fade_margin
    /// Left unset, the reply falls back to the flat default budget so that
    /// every URL that worked before still returns the same number. Set either
    /// one and the budget is DERIVED, which is the point: the shipped
    /// `GENERIC_SX1262` gain is a placeholder for a stock whip, and an
    /// operator running a 5.8 dBi collinear was previously unable to say so.
    tx_gain_dbi: Option<f64>,
    rx_gain_dbi: Option<f64>,
    /// Percentage of locations the loss is not exceeded at, pL of P.1812
    /// §4.7 eq. (69), both ways of the both-way mean. Left unset, the model's
    /// own (`LinkParams::eu868_defaults`, 90), so every URL that worked before
    /// returns the number it did. A caller that lays its own location spread
    /// over the answer, as sim-mesh's shadowing does, asks for the median: at
    /// 90 the spread is counted twice.
    loc_pct: Option<f64>,
    /// Leave out the profile drawn for the page (240 points, ~40 KB): a
    /// caller that keeps only the loss and its verdicts, such as sim-mesh's
    /// loss tables asking every pair, has no use for it. Everything else in
    /// the reply is the same.
    #[serde(default)]
    lean: bool,
}

/// The location percentage a link is judged at: the caller's, held to
/// P.1812's own range (`p1812::lb_from_arrays` refuses anything outside
/// 1..=99), else the model's default.
///
/// Checked here rather than left to the model, whose refusal is no 4xx: on a
/// path P.1812 judges it is the server's error (`link_model`), and inside the
/// 0.25 km floor the near-field model answers, which has no pL, so a bad one
/// would pass unremarked.
fn resolve_loc_pct(loc_pct: Option<f64>) -> Result<f64, String> {
    match loc_pct {
        None => Ok(planner_core::model::LinkParams::eu868_defaults().loc_pct),
        Some(p) if (1.0..=99.0).contains(&p) => Ok(p),
        Some(p) => Err(format!("loc_pct must be in [1, 99], not {p}")),
    }
}

/// The model a link reply names when P.1812 answered it.
const P1812_MODEL: &str = "ITU-R P.1812-8";

/// The loss a link reply gives, and the model it came from: P.1812's own
/// answer, or on a path inside its 0.25 km floor the near-field model's.
///
/// Only that refusal is answered from the near field, the one
/// `p1812::lb_from_arrays` gives a path shorter than 0.25 km (§1). Any other
/// is P.1812 refusing a path it does judge, and answering it from the near
/// field would give a free-space figure, labelled as a path inside the
/// floor, for one that is not; it is an error, and the reply says so.
fn link_model(
    p1812: Result<f64, planner_core::model::ModelError>,
    d_total_km: f64,
    near_field: impl FnOnce() -> Option<f64>,
) -> Result<(f64, &'static str), (StatusCode, String)> {
    match p1812 {
        Ok(lb) => Ok((lb, P1812_MODEL)),
        Err(planner_core::model::ModelError::OutOfRange(_)) if d_total_km < 0.25 => near_field()
            .map(|v| (v, "free space + P.526 diffraction (inside P.1812's 0.25 km floor)"))
            .ok_or_else(|| (StatusCode::BAD_REQUEST, "path too short to evaluate".into())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// The budget a link reply was judged against, and what it is made of.
struct Budget {
    budget_db: f32,
    source: &'static str,
    tx_gain_dbi: f64,
    rx_gain_dbi: f64,
    gains_given: bool,
}

/// Planning fade margin the derived budget carries. Same 10 dB the presets
/// endpoint quotes, named once so the two cannot drift apart.
const LINK_FADE_MARGIN_DB: f64 = 10.0;

/// Resolve the budget from what the caller actually said.
///
/// Three cases, in priority order:
///   1. an explicit `budget_db` wins — that caller has done the arithmetic;
///   2. otherwise, if EITHER antenna was named, derive from both;
///   3. otherwise the historical flat default, so every URL that worked before
///      returns the number it always did.
///
/// Case 2 is the new one. The shipped `GENERIC_SX1262` gain is a documented
/// placeholder for a stock whip, applied to BOTH ends, so an operator running
/// a 5.8 dBi collinear at one end previously had no way to say so and planned
/// with 3.8 dB less budget than they had.
fn resolve_budget(
    budget_db: Option<f32>,
    tx_gain_dbi: Option<f64>,
    rx_gain_dbi: Option<f64>,
) -> Budget {
    let dev = &planner_core::preset::GENERIC_SX1262;
    let radio = &planner_core::preset::MC_EU868_DEFAULT;
    let tx = tx_gain_dbi.unwrap_or(dev.antenna_gain_dbi);
    let rx = rx_gain_dbi.unwrap_or(dev.antenna_gain_dbi);
    let gains_given = tx_gain_dbi.is_some() || rx_gain_dbi.is_some();
    let derived = planner_core::preset::max_path_loss_asym(
        dev.tx_power_dbm,
        tx,
        rx,
        radio,
        LINK_FADE_MARGIN_DB,
    );
    let (b, source) = match (budget_db, gains_given, derived) {
        (Some(b), _, _) => (b, "budget_db given by the caller"),
        (None, true, Some(d)) => {
            (d as f32, "derived: tx_power + tx_gain + rx_gain - sensitivity - fade_margin")
        }
        _ => (d_budget(), "flat default (no antennas named)"),
    };
    Budget { budget_db: b, source, tx_gain_dbi: tx, rx_gain_dbi: rx, gains_given }
}

/// Point-to-point link between two chosen sites.
///
/// The coverage map is point-to-AREA at ONE receiver height, so it answers
/// "who hears me at 2 m above ground?". It structurally cannot show a link to
/// an elevated repeater on a hill — the case that prompted this endpoint: a
/// 12.8 km path to Teufelsberg that works in the field and appears nowhere on
/// a 2 m-receiver map. Here both ends carry their own antenna height, and the
/// terrain profile comes back with the answer so the margin is auditable
/// rather than a single colour on a raster.
/// `/height.json`'s query: a point in the pack's CRS, as `/link.json`'s ends.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HeightQuery {
    x: f64,
    y: f64,
}

/// `/height.json`: the antenna height a node standing at (x, y) most
/// plausibly has when nobody measured it. It is planner's estimator for a
/// deployed node whose advert gave none
/// (`planner_coverage::environment::estimate_height`), on this pack's
/// evidence: a LoD2 roof under the node with a mast on it, else the clutter
/// over its neighbourhood, else its land class's convention, else a
/// documented fallback. Each answer carries the band it believes and what it
/// rested on. While the building index is still loading no roof is asked,
/// and the reply says so, as `/link.json`'s does.
async fn height_json(State(st): State<Arc<AppState>>, Query(q): Query<HeightQuery>) -> Response {
    let p = Xy { x: q.x, y: q.y };
    if !(p.x.is_finite() && p.y.is_finite())
        || p.x < st.extent.min_x
        || p.x > st.extent.max_x
        || p.y < st.extent.min_y
        || p.y > st.extent.max_y
    {
        return (StatusCode::BAD_REQUEST, "the point is outside the pack").into_response();
    }
    match tokio::task::spawn_blocking(move || estimate_height_at(&st, p)).await {
        Ok(body) => ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The estimate at `p`, with the windows and buildings it needs read here.
/// Blocking.
fn estimate_height_at(st: &AppState, p: Xy) -> serde_json::Value {
    use planner_coverage::environment as env;
    let params = env::EstimateParams::default();
    // The neighbourhood the clutter tier samples, and a cell to spare.
    let reach = params.clutter_radius_m.max(params.building_search_radius_m) + 2.0 * terrain_res_hint(st);
    let lo = st.clamp(Xy { x: p.x - reach, y: p.y - reach });
    let hi = st.clamp(Xy { x: p.x + reach, y: p.y + reach });
    let clutter = st.layers.clutter.as_ref().and_then(|l| l.window(lo, hi).ok());
    let classes = st.layers.classes.as_ref().and_then(|l| l.window(lo, hi).ok());
    let (index_state, hints) = {
        let guard = st.buildings.read().unwrap_or_else(|e| e.into_inner());
        let hints = guard
            .get()
            .map(|ix| ix.hints_near(p.x, p.y, params.building_search_radius_m))
            .unwrap_or_default();
        (guard.label(), hints)
    };
    let ev = env::SiteEvidence { clutter: clutter.as_ref(), classes: classes.as_ref(), buildings: &hints };
    let e = env::estimate_height(p, None, &ev, &params);
    let detail = match &e.basis {
        env::HeightBasis::Lod2Building { footprint_m2, building_h_m, distance_m } => serde_json::json!({
            "footprint_m2": footprint_m2, "building_h_m": building_h_m, "distance_m": distance_m,
        }),
        env::HeightBasis::ClutterNeighbourhood { radius_m, percentile, sampled_cells } => serde_json::json!({
            "radius_m": radius_m, "percentile": percentile, "sampled_cells": sampled_cells,
        }),
        env::HeightBasis::ClassTypical(class) => serde_json::json!({ "class": format!("{class:?}") }),
        env::HeightBasis::OperatorSupplied | env::HeightBasis::NoEvidence => serde_json::json!({}),
    };
    serde_json::json!({
        "h_agl_m": e.h_agl_m,
        "low_m": e.low_m,
        "high_m": e.high_m,
        "basis": e.basis.kind().label(),
        "detail": detail,
        "clamped_from_m": e.clamped_from_m,
        "buildings_index": index_state,
    })
}

/// A pair's two ends as `/link.json` takes them, held into the pack.
#[derive(Clone, Copy)]
struct LinkEnds {
    a: Xy,
    b: Xy,
    dist: f64,
    loc_pct: f64,
}

/// What refuses a pair before any of its ground is read, checked in the
/// order `/link.json` always checked it.
fn link_ends(st: &AppState, q: &LinkQuery) -> Result<LinkEnds, (StatusCode, String)> {
    let loc_pct = resolve_loc_pct(q.loc_pct).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let a = st.clamp(Xy { x: q.ax, y: q.ay });
    let b = st.clamp(Xy { x: q.bx, y: q.by });
    let dist = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    // No longer a refusal. P.1812 is not valid below 0.25 km and this used to
    // stop there, which made the tool useless for the neighbour-to-neighbour
    // hop a mesh is actually built from; the near-field model below answers
    // it instead, and the reply says which model produced the number.
    //
    // A floor still exists, because free space diverges at zero range and a
    // profile of two points has no obstacle to diffract over.
    if dist < 20.0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "the two ends are within 20 m of each other — there is no path to model".into(),
        ));
    }
    if dist > MAX_COVERAGE_RADIUS_KM * 2.0 * 1000.0 {
        return Err((StatusCode::BAD_REQUEST, "path longer than the pack window cap".into()));
    }
    Ok(LinkEnds { a, b, dist, loc_pct })
}

async fn link_json(State(st): State<Arc<AppState>>, Query(q): Query<LinkQuery>) -> Response {
    let ends = match link_ends(&st, &q) {
        Ok(ends) => ends,
        Err(refused) => return refused.into_response(),
    };
    let Ok(_permit) = st.link_slots.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "every link slot is busy").into_response();
    };
    // The whole pair off the async workers, not only its reads and its
    // P.1812 runs: the profile walks the building index at every sample, and
    // run on a worker it kept the handlers of tiles from being polled while
    // a pack table filled.
    let reply = tokio::task::block_in_place(|| {
        // The windows over the pair's box, read for it alone.
        let windows = LinkWindows::read(&st.layers, link_box(&st, ends))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        // Read-locked ONCE for the whole profile, not per sample: forty
        // thousand lock acquisitions would cost more than the lookups, and —
        // more to the point — a profile half-sampled before the background
        // index lands and half after would mix two obstacle sources with one
        // `from_lod2` count to describe them. One guard means one answer from
        // one state.
        let index = st.buildings.read().expect("buildings lock");
        link_reply(&st, &q, ends, windows.views(), index.get(), index.label())
    });
    match reply {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// The box `/link.json` reads for a pair: both ends and four cells around
/// them, held into the pack.
fn link_box(st: &AppState, ends: LinkEnds) -> (Xy, Xy) {
    let LinkEnds { a, b, .. } = ends;
    let res = terrain_res_hint(st).max(1.0);
    (
        st.clamp(Xy { x: a.x.min(b.x) - res * 4.0, y: a.y.min(b.y) - res * 4.0 }),
        st.clamp(Xy { x: a.x.max(b.x) + res * 4.0, y: a.y.max(b.y) + res * 4.0 }),
    )
}

/// The four layers a pair's profile samples, each as the window over the
/// pair's box: terrain, clutter, building top, built fraction.
#[derive(Clone, Copy)]
struct LinkViews<'a> {
    terrain: planner_terrain::GridView<'a>,
    clutter: Option<planner_terrain::GridView<'a>>,
    building_top: Option<planner_terrain::GridView<'a>>,
    built_fraction: Option<planner_terrain::GridView<'a>>,
}

/// `/link.json`'s reply for a pair `link_ends` let through, from its layers'
/// windows and the building index as it stands, `bldg` (`None` while it
/// loads or where the pack has none), whose state is `index_label`.
/// Blocking.
fn link_reply(
    st: &AppState,
    q: &LinkQuery,
    ends: LinkEnds,
    views: LinkViews<'_>,
    bldg: Option<&BuildingIndex>,
    index_label: &'static str,
) -> Result<serde_json::Value, (StatusCode, String)> {
    // ---- the budget, and where every decibel of it comes from --------------
    //
    // Three cases, in order: an explicit `budget_db` wins; otherwise, if the
    // caller named either antenna, the budget is computed from the two of them;
    // otherwise the historical flat default, so old URLs keep their numbers.
    let dev = &planner_core::preset::GENERIC_SX1262;
    let radio = &planner_core::preset::MC_EU868_DEFAULT;
    let Budget {
        budget_db,
        source: budget_source,
        tx_gain_dbi: tx_gain,
        rx_gain_dbi: rx_gain,
        gains_given,
    } = resolve_budget(q.budget_db, q.tx_gain_dbi, q.rx_gain_dbi);
    let LinkEnds { a, b, dist, loc_pct } = ends;
    // The two unblended layers. `building_top` is the obstacle a path
    // crossing a building actually meets; `built_fraction` says how much of
    // the cell that building covers, which is what separates "this sample is
    // inside a Vorderhaus" from "this sample is on the street beside one".
    let LinkViews { terrain, clutter, building_top, built_fraction } = views;
    let res = terrain_res_hint(st).max(1.0);

    // Sample at HALF the raster cell for this one path. A point-to-point link
    // is a single profile, not a whole sweep, so the extra samples are free —
    // and stepping at exactly the cell size can stride over a building row
    // that a half-step catches. That reasoning is sound for the profile this
    // endpoint DRAWS and for the Fresnel clearance block below, and it is
    // wrong for the profile P.1812 is given: see the decimation after this
    // loop, which builds the model's own profile at §3.2.1's spacing.
    let step_m = (res * 0.5).max(1.0);
    let steps = ((dist / step_m).round() as usize).clamp(3, 40_000);
    let mut d_km = Vec::with_capacity(steps + 1);
    let mut h_masl = Vec::with_capacity(steps + 1);
    let mut g_masl = Vec::with_capacity(steps + 1);
    let mut clut = Vec::with_capacity(steps + 1);
    // How the obstacle at each sample was decided, so the answer can say what
    // it rests on rather than presenting averaged and surveyed alike.
    let (mut from_building, mut from_raster) = (0usize, 0usize);
    // Where each sample sits relative to a facade. Reported so a caller can
    // see whether this path runs down a street or through buildings — the
    // same number of decibels means very different things in the two cases.
    let (mut on_open, mut on_building, mut on_facade) = (0usize, 0usize, 0usize);
    // Per-sample, so the WORST pinch can be attributed to a building or to
    // bare ground. The advice differs completely: a rooftop can be gone round
    // or over, a hill cannot.
    let mut bf_at: Vec<f32> = Vec::with_capacity(steps + 1);
    // An end whose antenna is inside a building: its own building is not an
    // obstacle on the path out of it (the walls are its entry loss, below).
    let f_ghz = planner_core::model::LinkParams::eu868_defaults().freq_mhz / 1000.0;
    let indoor_end = |xy: Xy, agl: f64| -> Option<Indoor> {
        let ground = terrain.sample_bilinear(xy)? as f64;
        bldg.and_then(|ix| indoor_terminal(ix, xy.x, xy.y, ground + agl.clamp(1.0, 300.0), f_ghz))
    };
    let (indoor_a, indoor_b) = (indoor_end(a, q.tx_h), indoor_end(b, q.rx_h));
    let in_own = |p: Xy| {
        [indoor_a, indoor_b].iter().flatten().any(|d| bldg.is_some_and(|ix| ix.holds(d.building, p.x, p.y)))
    };
    for k in 0..=steps {
        let t = k as f64 / steps as f64;
        let p = Xy { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
        let Some(h) = terrain.sample_bilinear(p) else {
            return Err((StatusCode::BAD_REQUEST, "path leaves the pack".into()));
        };
        let h = h as f64;
        let raster_c =
            clutter.as_ref().and_then(|g| g.sample_bilinear(p)).unwrap_or(0.0).max(0.0) as f64;
        // A surveyed building beats the cell mean. The mean has already been
        // diluted by whatever else shares the cell — courtyard, street, garden
        // — so on a path that genuinely crosses the building it understates the
        // obstacle, and on one that passes beside it, it overstates.
        // How much of this cell is building at all. 0 = street, courtyard,
        // park or water; 1 = building interior; between = the sample sits on
        // a facade, where one averaged height describes neither side.
        let bf = built_fraction
            .as_ref()
            .and_then(|g| g.sample_bilinear(p))
            .unwrap_or(f32::NAN);
        bf_at.push(bf);
        if bf.is_finite() {
            if bf <= 0.001 {
                on_open += 1;
            } else if bf >= 0.999 {
                on_building += 1;
            } else {
                on_facade += 1;
            }
        }
        let c = match bldg.and_then(|b| b.top_at(p.x, p.y)) {
            _ if in_own(p) => 0.0,
            Some(top) => {
                from_building += 1;
                (top as f64 - h).max(0.0).max(raster_c)
            }
            None => {
                // No footprint under this sample. `building_top` is still a
                // better obstacle than the blended clutter where the cell has
                // any building in it, because clutter has already been scaled
                // DOWN by the built fraction — a real 22 m Vorderhaus in a
                // 10%-built cell reaches the profile as 1.5 m. Only used when
                // the polygons genuinely miss, which on a geometry pack means
                // the path really is off the footprint.
                from_raster += 1;
                let bt = building_top
                    .as_ref()
                    .and_then(|g| g.sample_bilinear(p))
                    .unwrap_or(0.0)
                    .max(0.0) as f64;
                if bf.is_finite() && bf >= 0.999 { raster_c.max(bt) } else { raster_c }
            }
        };
        d_km.push(dist / 1000.0 * t);
        h_masl.push(h);
        g_masl.push(h + c);
        clut.push(c);
    }

    // ---- The profile P.1812 actually gets ----------------------------------
    //
    // Not the one above. §3.2.1 sets a floor of the order of 30 m on the
    // spacing of a terrain-plus-representative-clutter profile, and the floor
    // is what makes eq. (13) mean anything: that equation maximises
    // (g_i + bulge − h_tc)/d_i over the INTERMEDIATE points, so a point's grip
    // on the answer is 1/d_i and grows without bound towards a terminal.
    // Sampling a 10 m raster every 5 m puts the first intermediate point 5 m
    // from the mast, still inside the transmitter's own clutter cell — the
    // model is then asked to diffract the transmitter over its own roof, with
    // 200 (m/km) of slope per metre of antenna height. It wins the maximisation
    // until the antenna clears that roof and then abruptly stops: measured on a
    // 1.155 km path over this pack, L_b ran 137.21 dB at a 14.50 m mast down to
    // 125.50 at 15.00 m — the last 2 cm of that costing 98 dB/m — and then went
    // flat. Every azimuth broke at the same height, because every radial leaves
    // the site through the same clutter cell.
    //
    // So: keep the fine samples for the drawing and the Fresnel block, and
    // decimate onto the model's floor here. Both terminals are kept exactly,
    // and the last interior point is dropped when it would leave a final
    // interval under the floor — eq. (17) has the same 1/(d − d_i) sharpness
    // at the receiver end that eq. (13) has at the transmitter.
    //
    // The stride comes off the REALISED sample gap, not the requested
    // `step_m`: `steps` is rounded, so dist/steps can land just under the
    // request and a stride derived from it would miss the floor by centimetres
    // — which the model would then (rightly) refuse.
    // Which §3.2 profile this actually IS, decided by where the obstacle
    // heights came from rather than declared.
    //
    // §3.2.1 eq. (1d) is terrain plus a REPRESENTATIVE clutter height looked
    // up per clutter class — a five-row table — and carries a 30 m spacing
    // floor. §3.2.2 eq. (1e) is surface heights taken DIRECTLY from surface
    // data and floors at 10 m instead.
    //
    // This profile is built from real LoD2 roof heights wherever a footprint
    // covers the sample and from the clutter raster elsewhere, so which
    // recommendation applies is a property of the path, not of the code. The
    // pack is 38.5% real 1 m lidar and 47.7% Table-2 class defaults; calling
    // the whole thing §3.2.2 would claim measured obstacles over half a city
    // that has none, and calling it all §3.2.1 throws away the resolution we
    // actually have — which is what was happening, and it is why an obstacle
    // 13 m from a terminal could never appear.
    let measured = from_building as f64 / (steps + 1) as f64;
    let method = if measured >= 0.5 {
        planner_propag::p1812::SurfaceMethod::SurfaceHeights
    } else {
        planner_propag::p1812::SurfaceMethod::RepresentativeClutter
    };
    let stride = method.stride_for(dist / steps as f64);
    // The profile the model gets, decimated from its transmitter end.
    let decimate = |h: &[f64], g: &[f64]| -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut md = Vec::with_capacity(steps / stride + 2);
        let mut mh = Vec::with_capacity(steps / stride + 2);
        let mut mg = Vec::with_capacity(steps / stride + 2);
        let last_interior = steps.saturating_sub(stride);
        let mut k = 0usize;
        while k <= last_interior {
            md.push(d_km[k]);
            mh.push(h[k]);
            // (1d): the terminals keep bare terrain — a terminal is not an
            // obstacle to itself. Inert today (the diffraction loops read
            // intermediate points only) and cheap to keep true.
            mg.push(if k == 0 { h[k] } else { g[k] });
            k += stride;
        }
        md.push(d_km[steps]);
        mh.push(h[steps]);
        mg.push(h[steps]);
        (md, mh, mg)
    };
    let (md_km, mh_masl, mg_masl) = decimate(&h_masl, &g_masl);
    // The same path the other way round, decimated from ITS transmitter.
    let rev_h: Vec<f64> = h_masl.iter().rev().copied().collect();
    let rev_g: Vec<f64> = g_masl.iter().rev().copied().collect();
    let (rd_km, rh_masl, rg_masl) = decimate(&rev_h, &rev_g);
    if md_km.len() < 3 {
        return Err((StatusCode::BAD_REQUEST, "path too short for a §3.2 profile".into()));
    }

    let (_, lat) = st.to_lonlat(Xy { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 });
    let mut link = planner_core::model::LinkParams::eu868_defaults();
    link.tx_h_agl_m = q.tx_h.clamp(1.0, 300.0);
    link.rx_h_agl_m = q.rx_h.clamp(1.0, 300.0);
    link.delta_n = st.manifest.region.delta_n;
    link.n0 = st.manifest.region.n0;
    link.path_center_lat_deg = lat;
    link.loc_pct = loc_pct;

    let d_total = dist / 1000.0;
    let x = planner_propag::p1812::ArrayInputs {
        d_km: &md_km,
        h_masl: &mh_masl,
        g_masl: &mg_masl,
        surface_method: method,
        omega: 0.0,
        dct_km: d_total,
        dcr_km: d_total,
        d_tm_km: d_total,
        d_lm_km: d_total,
        r_rx_m: *clut.last().unwrap_or(&0.0),
    };
    let x_rev = planner_propag::p1812::ArrayInputs {
        d_km: &rd_km,
        h_masl: &rh_masl,
        g_masl: &rg_masl,
        surface_method: method,
        omega: 0.0,
        dct_km: d_total,
        dcr_km: d_total,
        d_tm_km: d_total,
        d_lm_km: d_total,
        r_rx_m: *clut.first().unwrap_or(&0.0),
    };
    // ---- what P.1812 does not model: the terminals' own surroundings ------
    //
    // Checked against refs/P1812-8.pdf: 36 pages, zero occurrences of
    // "P.2108", "height gain" or "terminal surroundings". P.1812 puts both
    // terminals on BARE TERRAIN by construction, in either profile method and
    // at any spacing, so a node on a balcony below its own roofline gets a
    // free horizon from it. That is faithful P.1812; the missing loss lives
    // in P.2108-1 §3.1, and this is where it is added.
    //
    // R and w_s are MEASURED along the path bearing from the LoD2 footprints
    // rather than taken from the Recommendation's nominal 27 m street width.
    // With the nominal constant the correction is the same in every direction
    // from a site — an offset, which cannot produce a directional shadow and
    // cannot improve a ranking. Measured, it differs bearing by bearing.
    let bearing = (b.x - a.x).atan2(b.y - a.y);
    let (tx_ground, rx_ground) = (h_masl[0], h_masl[steps]);
    let near = |xy: Xy, ground: f64, agl: f64, brg: f64| -> Option<(f64, f64)> {
        bldg.and_then(|ix| {
            ix.clutter_along(xy.x, xy.y, ground, ground + agl, brg, 120.0, 2.0)
        })
    };
    // An indoor end has no surroundings in P.2108's sense (it is not a
    // terminal in the open among clutter), and is not raised onto its roof:
    // its walls are its building entry loss instead.
    let tx_clut = if indoor_a.is_some() { None } else { near(a, tx_ground, link.tx_h_agl_m, bearing) };
    // The receiver looks back down the same path, hence the reversed bearing.
    let rx_clut = if indoor_b.is_some() {
        None
    } else {
        near(b, rx_ground, link.rx_h_agl_m, bearing + std::f64::consts::PI)
    };
    // The antennas' own heights, as asked; the model may run higher (below).
    let (antenna_tx_m, antenna_rx_m) = (link.tx_h_agl_m, link.rx_h_agl_m);

    use planner_propag::p2108::{height_gain_correction, model_height_m, TerminalClutter};
    let ah = |agl: f64, c: Option<(f64, f64)>, indoor: Option<Indoor>| -> f64 {
        if let Some(d) = indoor {
            return d.entry_db;
        }
        c.and_then(|(r, ws)| {
            height_gain_correction(link.freq_mhz / 1000.0, agl, r, ws, TerminalClutter::Obstructed)
        })
        .unwrap_or(0.0)
    };
    let ah_tx = ah(link.tx_h_agl_m, tx_clut, indoor_a);
    let ah_rx = ah(link.rx_h_agl_m, rx_clut, indoor_b);

    // P.2108 corrects a loss computed TO THE CLUTTER HEIGHT, not to the
    // antenna. Running P.1812 at the antenna and adding A_h charges twice for
    // the same obstruction: the model has already given the low antenna a
    // worse path. So raise each terminal to its own R for the P.1812 call.
    let mut link = link;
    if let Some((r, _)) = tx_clut {
        link.tx_h_agl_m = model_height_m(link.tx_h_agl_m, r);
    }
    if let Some((r, _)) = rx_clut {
        link.rx_h_agl_m = model_height_m(link.rx_h_agl_m, r);
    }

    // A path loses the same both ways, and P.1812 as run here does not: the
    // location variability is the receiver's alone, and the profile is
    // decimated from the transmitter, so the two directions of one pair came
    // out up to 14 dB apart over Mitte. So the model is run both ways and the
    // pair gets the mean, the same whichever end asks.
    let mut link_rev = link.clone();
    std::mem::swap(&mut link_rev.tx_h_agl_m, &mut link_rev.rx_h_agl_m);
    let t0 = std::time::Instant::now();
    let loss = planner_propag::p1812::lb_from_arrays(&x, &link).and_then(|fwd| {
        let rev = planner_propag::p1812::lb_from_arrays(&x_rev, &link_rev)?;
        Ok(planner_core::model::Loss { lb_db: (fwd.lb_db + rev.lb_db) / 2.0 })
    });
    let ms = t0.elapsed().as_millis();
    // Under 0.25 km P.1812 refuses (§1). That refusal is not an answer: the
    // first 250 m of a city is where the courtyard wall and the block
    // opposite are, and a measure-link tool that returns an error there is
    // useless for exactly the neighbour-to-neighbour hop a mesh is built
    // from. Free space plus diffraction over the real roofs on the path.
    // Both ways here too, for the same reason.
    let near_field = || {
        planner_propag::near_field::loss_db(&planner_propag::near_field::NearFieldPath {
            d_km: &md_km,
            h_masl: &mh_masl,
            g_masl: &mg_masl,
            f_mhz: link.freq_mhz,
            tx_h_agl_m: link.tx_h_agl_m,
            rx_h_agl_m: link.rx_h_agl_m,
        })
        .zip(planner_propag::near_field::loss_db(&planner_propag::near_field::NearFieldPath {
            d_km: &rd_km,
            h_masl: &rh_masl,
            g_masl: &rg_masl,
            f_mhz: link_rev.freq_mhz,
            tx_h_agl_m: link_rev.tx_h_agl_m,
            rx_h_agl_m: link_rev.rx_h_agl_m,
        }))
        .map(|(f, r)| (f + r) / 2.0)
    };
    let (lb_p1812, model_used) = link_model(loss.map(|l| l.lb_db), d_total, near_field)?;
    let lb = lb_p1812 + ah_tx + ah_rx;

    // ---- Fresnel clearance -------------------------------------------------
    //
    // P.1812 already prices diffraction into lb_db, but a single loss figure
    // does not tell an operator WHERE the path is pinched or by how much, and
    // "it closes today" is a fragile answer if the path grazes a rooftop: a
    // crane, a growing tree or a new storey takes it away. The first Fresnel
    // zone is the standard way to express that structural margin.
    //
    // Radius of the first Fresnel zone at a point splitting the path d1/d2:
    //     r1 = sqrt(lambda * d1 * d2 / (d1 + d2))
    // Obstacles are raised by the earth's bulge relative to the straight ray,
    // using the EFFECTIVE earth radius: k = 157 / (157 - dN) with dN the
    // refractivity lapse from the pack manifest (the same quantity P.1812
    // uses), rather than a hardcoded 4/3.
    let lambda_m = 299.792_458 / link.freq_mhz; // c[m/us]/f[MHz] -> metres
    let k_factor = 157.0 / (157.0 - link.delta_n).max(1e-6);
    let a_e_m = k_factor * 6_371_000.0;
    // The real ray, between the antennas where they are.
    let tx_masl = h_masl[0] + antenna_tx_m;
    let rx_masl = h_masl[steps] + antenna_rx_m;

    // How close to a terminal a sample may be and still count as THE
    // obstruction.
    //
    // A fixed 30 m was arbitrary and left the artefact in place: on a 7.4 km
    // path the worst pinch still landed 50 m from the mast at -503%, which is
    // the transmitter's own street, not a feature of the link.
    //
    // The honest limit comes from the data. The first Fresnel radius near a
    // terminal grows as sqrt(lambda*d1), so close in it is NARROWER THAN ONE
    // RASTER CELL — at 869 MHz on a 5 m pack it only reaches 5 m at 73 m out.
    // Inside that, the clearance is a difference between two numbers that the
    // pack cannot resolve from each other, and reporting it as the worst pinch
    // states a precision the pack does not have. P.1812 already prices
    // near-terminal clutter separately, through the terminal's own
    // representative clutter height, so counting it here also double-counts
    // it.
    //
    // Floored at §3.2.1's ~30 m so a very fine pack cannot drop below the
    // spacing the model itself demands.
    let terminal_exclusion_m = (res * res / lambda_m).max(30.0);
    let mut ratios = Vec::with_capacity(steps + 1);
    let (mut worst_ratio, mut worst_i) = (f64::INFINITY, 0usize);
    for k in 0..=steps {
        let d1 = d_km[k] * 1000.0;
        let d2 = dist - d1;
        // Endpoints have a zero-width zone; a ratio there is meaningless, so
        // they are recorded but never allowed to win "worst".
        let r1 = if d1 <= 0.0 || d2 <= 0.0 {
            0.0
        } else {
            (lambda_m * d1 * d2 / dist).sqrt()
        };
        let bulge = d1 * d2 / (2.0 * a_e_m);
        let ray = tx_masl + (rx_masl - tx_masl) * (d1 / dist);
        // Clutter counts: the thing blocking a household link is usually a
        // building or a treeline, not bare earth.
        let clearance = ray - (g_masl[k] + bulge);
        let ratio = if r1 > 0.0 { clearance / r1 } else { f64::INFINITY };
        // A sample a few metres from a mast is inside that terminal's OWN
        // clutter cell: the antenna is modelled as standing in the middle of
        // its own building, so the clearance there is hugely negative and it
        // wins "worst" on every urban path. Reported worst pinches were
        // landing at 0.00 km with -380% on links that are otherwise fine,
        // which says nothing an operator can act on.
        //
        // A terminal does not diffract over itself, and inside the exclusion
        // the Fresnel zone is narrower than a pack cell anyway. Samples in
        // there stay in `ratios` for the drawn profile; they just cannot be
        // the answer.
        let near_terminal = d1 < terminal_exclusion_m || d2 < terminal_exclusion_m;
        if r1 > 0.0 && !near_terminal && ratio < worst_ratio {
            worst_ratio = ratio;
            worst_i = k;
        }
        ratios.push((r1, clearance, ratio, ray, bulge));
    }
    if !worst_ratio.is_finite() {
        worst_ratio = 0.0;
    }
    // 0.6 x F1 is the long-standing engineering threshold for "behaves like
    // free space"; below 0 the ray is physically obstructed.
    let verdict = if worst_ratio >= 0.6 {
        "clear"
    } else if worst_ratio >= 0.0 {
        "grazing"
    } else {
        "obstructed"
    };

    // ---- what to actually DO about this link -------------------------------
    //
    // A clearance percentage and a loss figure are a diagnosis, not a course
    // of action, and the right action differs per link: the same -63% is
    // nothing to worry about with 26 dB of margin and fatal with 3 dB. So the
    // options below are derived from THIS path, and the height sensitivity is
    // MEASURED rather than assumed -- the profile arrays do not depend on
    // terminal height, so re-running the model with one end raised costs one
    // more evaluation and gives the true dB per metre for this geometry.
    // Rules of thumb get this badly wrong: over a knife edge close to one
    // terminal, raising the far end can be worth almost nothing.
    let (base_tx_h, base_rx_h) = (link.tx_h_agl_m, link.rx_h_agl_m);
    let probe = |tx_d: f64, rx_d: f64| -> Option<f64> {
        let mut l2 = link.clone();
        l2.tx_h_agl_m = (base_tx_h + tx_d).clamp(1.0, 300.0);
        l2.rx_h_agl_m = (base_rx_h + rx_d).clamp(1.0, 300.0);
        planner_propag::p1812::lb_from_arrays(&x, &l2).ok().map(|l| l.lb_db)
    };
    const PROBE_M: f64 = 3.0;
    let db_per_m_tx = probe(PROBE_M, 0.0).map(|l| (lb - l) / PROBE_M).unwrap_or(0.0);
    let db_per_m_rx = probe(0.0, PROBE_M).map(|l| (lb - l) / PROBE_M).unwrap_or(0.0);
    let margin = budget_db as f64 - lb;

    // What the worst pinch is standing on.
    let pinch_bf = bf_at.get(worst_i).copied().unwrap_or(f32::NAN);
    let pinch_on = if !pinch_bf.is_finite() {
        "unknown ground"
    } else if pinch_bf >= 0.6 {
        "a building"
    } else if pinch_bf > 0.001 {
        "a building edge"
    } else {
        "open ground"
    };

    // Metres of extra height at one end to reach a target margin, using the
    // measured slope. `None` when that end buys nothing worth having.
    let metres_for = |need_db: f64, slope: f64| -> Option<f64> {
        if slope > 0.05 && need_db > 0.0 { Some(need_db / slope) } else { None }
    };

    let mut options: Vec<serde_json::Value> = Vec::new();
    let mut push = |what: &str, gain: f64, detail: String, easy: bool| {
        options.push(serde_json::json!({
            "action": what, "gain_db": (gain * 10.0).round() / 10.0,
            "detail": detail, "practical": easy,
        }));
    };

    if margin >= 12.0 {
        push(
            "nothing",
            0.0,
            format!(
                "{margin:.0} dB of margin. The obstruction costs what it costs and                  the link still closes; clearance only becomes the thing to fix                  when the margin stops covering it."
            ),
            true,
        );
        if worst_ratio < 0.6 {
            let slope = db_per_m_tx.max(db_per_m_rx);
            push(
                "watch",
                0.0,
                format!(
                    "The pinch is on {pinch_on} at {:.2} km. Another storey or a                      grown tree there costs roughly {:.0} dB, which this margin                      absorbs -- recheck if the far end changes.",
                    d_km[worst_i],
                    (slope * 3.0).abs().max(1.0)
                ),
                true,
            );
        }
    } else {
        // Aim for 12 dB of margin: enough that ordinary fading and a small
        // change at either end does not take the link away.
        let need = 12.0 - margin;
        for (label, slope, end) in [
            ("raise this end", db_per_m_tx, "transmitter"),
            ("raise the far end", db_per_m_rx, "far end"),
        ] {
            match metres_for(need, slope) {
                Some(m) if m <= 15.0 => push(
                    label,
                    need,
                    format!(
                        "+{m:.0} m at the {end} ({slope:.2} dB per metre here, measured                          on this path)."
                    ),
                    m <= 6.0,
                ),
                Some(m) => push(
                    label,
                    need,
                    format!(
                        "would need +{m:.0} m at the {end} -- not a mast, a building.                          Height is the wrong lever on this path."
                    ),
                    false,
                ),
                None => push(
                    label,
                    0.0,
                    format!(
                        "raising the {end} buys almost nothing here ({slope:.2} dB/m):                          the pinch is too close to the other terminal."
                    ),
                    false,
                ),
            }
        }
        push(
            "directional antenna",
            6.0,
            "A modest yagi at this end is about +6 dBi over a whip, and this is a              point-to-point link so the pattern costs nothing you were using."
                .into(),
            true,
        );
        push(
            "slower modulation",
            7.5,
            "SF8 to SF11 is about +7.5 dB of processing gain for 4x the airtime --              affordable on a backbone hop, not on a busy channel."
                .into(),
            true,
        );
        if margin < -12.0 {
            push(
                "relay",
                0.0,
                format!(
                    "{:.0} dB short with the pinch on {pinch_on}. No single change                      at either end covers that; this wants an intermediate site.",
                    -margin
                ),
                true,
            );
        }
    }

    // Downsample the profile for display. The Fresnel block above used every
    // fine sample; P.1812 used the §3.2.1-spaced decimation, and
    // `profile_evidence` reports both so the two are not confused.
    let want = if q.lean { 0 } else { 240usize.min(d_km.len()) };
    let denom = want.max(2) - 1;
    let profile: Vec<serde_json::Value> = (0..want)
        .map(|i| {
            let k = (i * (d_km.len() - 1) / denom).min(d_km.len() - 1);
            let (r1, clearance, _ratio, ray, bulge) = ratios[k];
            serde_json::json!({
                "d": d_km[k],
                "h": h_masl[k],
                "g": g_masl[k],
                // Earth curvature folded into the obstacle, so the client can
                // draw a straight ray and still be geometrically honest.
                "e": g_masl[k] + bulge,
                "ray": ray,
                "f1": r1,
                "clr": clearance,
            })
        })
        .collect();

    let mut reply = serde_json::json!({
        "distance_km": d_total,
        "lb_db": lb,
        "budget_db": budget_db,
        "margin_db": budget_db as f64 - lb,
        "closes": lb <= budget_db as f64,
        // The budget is an ASSUMPTION stack, not a measurement, and a reply
        // that prints only its total invites the reader to treat it as one.
        "budget_terms": {
            "source": budget_source,
            "tx_power_dbm": dev.tx_power_dbm,
            "tx_gain_dbi": tx_gain,
            "rx_gain_dbi": rx_gain,
            "sensitivity_dbm": planner_core::preset::sensitivity_dbm(
                radio.spreading_factor, radio.bw_khz),
            "fade_margin_db": LINK_FADE_MARGIN_DB,
            // True while the gains are the shipped placeholder for a stock
            // whip rather than anything anybody measured.
            "gains_are_placeholder": !gains_given,
        },
        // The antennas where they are; P.1812 may have been run from higher,
        // raised to the clutter around an end in the open (P.2108 §3.1).
        "tx_h": antenna_tx_m,
        "rx_h": antenna_rx_m,
        "model_tx_h": link.tx_h_agl_m,
        "model_rx_h": link.rx_h_agl_m,
        // An end inside a building, and what its walls cost.
        "tx_indoor_entry_db": indoor_a.map(|d| (d.entry_db * 10.0).round() / 10.0),
        "rx_indoor_entry_db": indoor_b.map(|d| (d.entry_db * 10.0).round() / 10.0),
        "compute_ms": ms,
        // What the obstacle profile actually rests on. A link whose samples are
        // all raster is a link over averaged, mostly synthesized clutter; one
        // backed by LoD2 is over surveyed roof heights. Same number, very
        // different confidence, and the caller must be able to tell.
        "profile_evidence": {
            "samples": steps + 1,
            "step_m": step_m,
            // What P.1812 was given, which is deliberately coarser than what
            // is drawn: §3.2.1 floors the spacing of a terrain-plus-clutter
            // profile at the order of 30 m, and below that floor eq. (13)
            // starts diffracting each terminal over its own clutter cell.
            "model_samples": md_km.len(),
            "model_step_m": dist / (md_km.len() - 1) as f64,
            "from_lod2_buildings": from_building,
            "from_clutter_raster": from_raster,
            "buildings_indexed": bldg.map(|b| b.count).unwrap_or(0),
            "with_real_footprints": bldg.map(|b| b.with_geometry).unwrap_or(0),
            // Where the profile ran, by the pack's own 1 m building mask.
            // A path that never leaves open ground and one that crosses six
            // Vorderhauser produce the same `lb_db` with very different
            // confidence, and nothing else in this reply distinguishes them.
            "samples_open_ground": on_open,
            "samples_building_interior": on_building,
            "samples_on_a_facade": on_facade,
            // WHY there were no buildings, which "0 indexed" alone cannot say.
            // A pack without a buildings layer and a pack whose index is still
            // being read both answer from the clutter raster and both look
            // identical in the counts above — but one is the best this pack can
            // do and the other is a number that will change in a few seconds.
            "buildings_index": index_label,
            // Which §3.2 branch this path was evaluated under, and therefore
            // which spacing floor applied. Reported because the two give
            // different answers and nothing else in the reply says which was
            // used.
            // Which model produced lb_db. Inside 0.25 km it is not P.1812 at
            // all, and presenting the two alike would hide that.
            "model": model_used,
            "p1812_profile_method": match method {
                planner_propag::p1812::SurfaceMethod::SurfaceHeights => "3.2.2 surface heights",
                planner_propag::p1812::SurfaceMethod::RepresentativeClutter =>
                    "3.2.1 representative clutter",
            },
            "measured_obstacle_fraction": (measured * 100.0).round() / 100.0,
        },
        // What to DO, derived from this path rather than from a rule of
        // thumb. `db_per_m_*` are measured by re-running the model with one
        // end raised, because over a knife edge close to one terminal the
        // far end can be worth almost nothing and a generic answer would
        // send the operator up the wrong building.
        // The half of the answer P.1812 does not model. Reported separately
        // from lb_db, never folded into it: P.1812's number stays exactly
        // what the Recommendation says and what the oracle validates, and a
        // reader can see how much of the total is terminal surroundings.
        "terminal_clutter": {
            "p1812_db": (lb_p1812 * 10.0).round() / 10.0,
            "a_h_this_end_db": (ah_tx * 10.0).round() / 10.0,
            "a_h_far_end_db": (ah_rx * 10.0).round() / 10.0,
            "this_end": tx_clut.map(|(r, w)| serde_json::json!({
                "roof_above_ground_m": (r * 10.0).round() / 10.0,
                "distance_m": (w * 10.0).round() / 10.0,
            })),
            "far_end": rx_clut.map(|(r, w)| serde_json::json!({
                "roof_above_ground_m": (r * 10.0).round() / 10.0,
                "distance_m": (w * 10.0).round() / 10.0,
            })),
            "model": "ITU-R P.2108-1 3.1, R and w_s measured from LoD2 footprints                       along this bearing rather than the nominal 27 m",
        },
        "options": options,
        "sensitivity": {
            "db_per_m_this_end": (db_per_m_tx * 100.0).round() / 100.0,
            "db_per_m_far_end": (db_per_m_rx * 100.0).round() / 100.0,
            "pinch_on": pinch_on,
            "pinch_built_fraction": if pinch_bf.is_finite() { serde_json::json!(pinch_bf) }
                                    else { serde_json::Value::Null },
        },
        "fresnel": {
            "lambda_m": lambda_m,
            "k_factor": k_factor,
            "worst_ratio": worst_ratio,
            "worst_d_km": d_km[worst_i],
            "worst_clearance_m": ratios[worst_i].1,
            "worst_f1_m": ratios[worst_i].0,
            "verdict": verdict,
        },
        "profile": profile,
    });
    if q.lean {
        if let Some(map) = reply.as_object_mut() {
            map.remove("profile");
        }
    }
    Ok(reply)
}

/// `POST /links.json`'s body: the nodes, and the pairs among them to answer.
/// `deny_unknown_fields` for `LinkQuery`'s reason: a misspelled field is
/// refused rather than answered with its default.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinksRequest {
    /// Each node as `[x, y, h]`: the pack's CRS metres, and its antenna's
    /// height above ground in metres, as `/link.json`'s `tx_h` or `rx_h`.
    ///
    /// Coordinates given to the millimetre, as the front gives
    /// `/link.json`'s (`%.3f`), are read here as `/link.json` reads them
    /// from its query. One of 17 significant digits can be read an ulp away:
    /// serde_json's default reading of long numbers is not correctly
    /// rounded, and its exact one would change how every JSON file here is
    /// read, the buildings among them.
    nodes: Vec<[f64; 3]>,
    /// The pairs, as `[a, b]` indices into `nodes`, `a` the end
    /// `/link.json` calls `a`.
    #[serde(default)]
    pairs: Vec<[u32; 2]>,
    /// Instead of `pairs`: this node against every other, in node order.
    from: Option<u32>,
    /// As `/link.json`'s.
    loc_pct: Option<f64>,
}

impl LinksRequest {
    /// The pairs asked for, each naming two of the nodes.
    fn pairs(&self) -> Result<Vec<[u32; 2]>, String> {
        let n = self.nodes.len();
        let pairs = match (self.from, self.pairs.is_empty()) {
            (Some(_), false) => return Err("name the `pairs` or a node `from`, not both".into()),
            (None, true) => return Err("name the `pairs` to answer, or a node `from`".into()),
            (Some(f), true) if f as usize >= n => {
                return Err(format!("node {f} is not among the {n} nodes"))
            }
            (Some(f), true) => (0..n as u32).filter(|&k| k != f).map(|k| [f, k]).collect(),
            (None, false) => self.pairs.clone(),
        };
        match pairs.iter().flatten().find(|&&i| i as usize >= n) {
            Some(bad) => Err(format!("node {bad} is not among the {n} nodes")),
            None => Ok(pairs),
        }
    }
}

/// Many pairs at once, each answered as `/link.json` answers it: a pack's
/// loss table, or one node against the rest.
///
/// A table asked `/link.json` twice for every pair, once each way, and each
/// ask read four windows over its pair's box: on the berlin-centre pack a
/// long pair's box is the whole pack. Here each pair is computed once, and
/// its loss is the table's both ways: `/link.json` gives a pair the mean of
/// P.1812 run in both directions plus both ends' terminal terms, the same
/// whichever end asks. The layers are read once for the batch, over the box
/// holding every pair's, and each pair samples the part its own window would
/// have held, at that window's origin (`GridView`), so its numbers are
/// `/link.json`'s for the same query, to the bit. Past `BATCH_SHARED_CELLS`
/// each pair reads its own windows, as many at once as one shared read would
/// hold. A batch takes one link slot, and its pairs run in the sweep pool,
/// never the tiles'.
///
/// Reply: `buildings_index` as `/link.json`'s (the batch waits out a loading
/// index, as a sweep does), `read` "once" or "per pair", `compute_ms`, and
/// one entry per pair in the order asked in each of `lb_db` (`/link.json`'s,
/// `null` for a pair it refuses) and `flags` (1: the near-field model
/// answered, inside P.1812's 0.25 km floor; 2: the Fresnel verdict is
/// "clear"). `refused` lists `[pair, status, text]`, what `/link.json`
/// answers that pair.
async fn links_json(
    State(st): State<Arc<AppState>>,
    axum::Json(req): axum::Json<LinksRequest>,
) -> Response {
    let pairs = match req.pairs() {
        Ok(pairs) => pairs,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    if let Err(e) = resolve_loc_pct(req.loc_pct) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    let Ok(_permit) = st.link_slots.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "every link slot is busy").into_response();
    };
    let st2 = Arc::clone(&st);
    match tokio::task::spawn_blocking(move || link_rows(&st2, &req, &pairs)).await {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `/links.json`'s reply. Blocking.
fn link_rows(st: &AppState, req: &LinksRequest, pairs: &[[u32; 2]]) -> serde_json::Value {
    let t0 = std::time::Instant::now();
    // Never a row from the clutter raster while the index loads: that is a
    // different number from the same row a moment later, and a table keeps
    // what it is given.
    let _ = wait_for_index(&st.buildings, &AtomicBool::new(false));
    let (index, index_label) = {
        let state = st.buildings.read().expect("buildings lock");
        (state.ready(), state.label())
    };
    let queries: Vec<LinkQuery> = pairs
        .iter()
        .map(|&[i, j]| {
            let ([ax, ay, tx_h], [bx, by, rx_h]) = (req.nodes[i as usize], req.nodes[j as usize]);
            LinkQuery {
                ax,
                ay,
                bx,
                by,
                tx_h,
                rx_h,
                budget_db: None,
                tx_gain_dbi: None,
                rx_gain_dbi: None,
                loc_pct: req.loc_pct,
                lean: true,
            }
        })
        .collect();
    let ends: Vec<_> = queries.iter().map(|q| link_ends(st, q)).collect();
    let union = ends.iter().flatten().map(|&e| link_box(st, e)).reduce(|(lo, hi), (l, h)| {
        (Xy { x: lo.x.min(l.x), y: lo.y.min(l.y) }, Xy { x: hi.x.max(h.x), y: hi.y.max(h.y) })
    });
    let shared = union.and_then(|bx| SharedLinkWindows::read(&st.layers, bx));
    let row = |k: usize| -> Result<serde_json::Value, (StatusCode, String)> {
        let ends = ends[k].clone()?;
        let bx = link_box(st, ends);
        match shared.as_ref().and_then(|s| s.views(bx)) {
            Some(views) => link_reply(st, &queries[k], ends, views, index.as_deref(), index_label),
            None => {
                let windows = LinkWindows::read(&st.layers, bx)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                link_reply(st, &queries[k], ends, windows.views(), index.as_deref(), index_label)
            }
        }
    };
    // Pairs reading their own windows hold them while they compute: no more
    // of them at once than would hold what one shared read holds.
    let at_once = if shared.is_some() {
        queries.len().max(1)
    } else {
        let cells = |&e: &LinkEnds| {
            let (lo, hi) = link_box(st, e);
            st.layers.terrain.meta.window_box(lo, hi).map_or(1, |(_, _, w, h)| w * h)
        };
        let most = ends.iter().flatten().map(cells).max().unwrap_or(1);
        (BATCH_SHARED_CELLS / most.max(1)).max(1)
    };
    let all: Vec<usize> = (0..queries.len()).collect();
    let rows: Vec<Result<serde_json::Value, (StatusCode, String)>> = st.sweep_pool.install(|| {
        all.chunks(at_once)
            .flat_map(|c| c.par_iter().map(|&k| row(k)).collect::<Vec<_>>())
            .collect()
    });
    let (mut lb_db, mut flags, mut refused) = (Vec::new(), Vec::new(), Vec::new());
    for (k, row) in rows.into_iter().enumerate() {
        match row {
            Ok(reply) => {
                let near_field = reply["profile_evidence"]["model"] != P1812_MODEL;
                let clear = reply["fresnel"]["verdict"] == "clear";
                lb_db.push(reply["lb_db"].clone());
                flags.push(u8::from(near_field) | u8::from(clear) << 1);
            }
            Err((status, text)) => {
                lb_db.push(serde_json::Value::Null);
                flags.push(0);
                refused.push(serde_json::json!([k, status.as_u16(), text]));
            }
        }
    }
    serde_json::json!({
        "buildings_index": index_label,
        "read": if shared.is_some() { "once" } else { "per pair" },
        "compute_ms": t0.elapsed().as_millis(),
        "lb_db": lb_db,
        "flags": flags,
        "refused": refused,
    })
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "d_search_limit")]
    limit: usize,
}
fn d_search_limit() -> usize {
    12
}

/// Offline place/street/postcode lookup against the pack's own gazetteer.
/// No geocoding service is contacted — that is the whole point.
async fn search(State(st): State<Arc<AppState>>, Query(q): Query<SearchQuery>) -> Response {
    let hits = st.places.search(&q.q, q.limit.clamp(1, 50));
    let out: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            let (lon, lat) = st.to_lonlat(Xy { x: h.x as f64, y: h.y as f64 });
            serde_json::json!({
                "kind": h.kind.label(),
                "name": h.name,
                "ctx": h.ctx,
                "x": h.x, "y": h.y,
                "lon": lon, "lat": lat,
                // Present only for postcodes: fetch /area.bin?i= to outline it.
                "area": if h.area == usize::MAX { serde_json::Value::Null }
                        else { serde_json::json!(h.area) },
            })
        })
        .collect();
    axum::Json(serde_json::json!({ "hits": out })).into_response()
}

#[derive(Deserialize)]
struct AreaQuery {
    i: usize,
}

/// One postal area's boundary rings, projected, for the browser to outline.
/// Format: "PAR1" | u16 ring_count | per ring: u32 n | n × (f32 x, f32 y)
async fn area_bin(State(st): State<Arc<AppState>>, Query(q): Query<AreaQuery>) -> Response {
    let Some(a) = st.places.areas.get(q.i) else {
        return (StatusCode::NOT_FOUND, "no such area").into_response();
    };
    let mut out = Vec::new();
    out.extend_from_slice(b"PAR1");
    out.extend_from_slice(&(a.rings.len() as u16).to_le_bytes());
    for ring in &a.rings {
        out.extend_from_slice(&(ring.len() as u32).to_le_bytes());
        for (x, y) in ring {
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
        }
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "max-age=3600"),
        ],
        out,
    )
        .into_response()
}

/// The deployed network, for markers and for the "what does a site here add"
/// question. Small enough (a few hundred nodes for a city) to ship as JSON in
/// one request and keep in the page.
///
/// Every node carries BOTH its projected pack-CRS metres (what the renderer
/// draws and what the sweep endpoints take) and its true lat/lon, projected
/// here with proj4rs. The lat/lon is what the page's nodes.csv export writes,
/// and it must be a real inverse projection: interpolating lon/lat across the
/// pack bbox is exact only at the corners and put the sweep centre hundreds of
/// metres off the pin once already. Projecting all ~270 of them costs well
/// under a millisecond, and doing it here means the page never makes a round
/// trip while an operator is typing a height.
/// One node as the page sees it. Pure, so the Option boundary below is
/// testable without a running server or an open pack.
fn node_json(n: &planner_pack::nodes::DeployedNode, lon: f64, lat: f64) -> serde_json::Value {
    // `label()` and not a local match: the page writes this string straight
    // into its nodes.csv export, so the only spelling that survives a
    // re-import is the one NodeKind::from_label() reads back. Restating the
    // table here is how the two drift apart.
    serde_json::json!({
        "id": n.id,
        "name": n.name,
        "kind": n.kind.label(),
        "x": n.x, "y": n.y,
        "lat": lat, "lon": lon,
        // null, not 0: nobody has measured these, and the whole gap answer
        // turns on them.
        "h": finite_or_null(n.height_agl_m),
        "tx_dbm": finite_or_null(n.tx_power_dbm),
        // A PRESENT but non-finite value is not the same thing as an absent
        // one, and serde_json writes both as JSON `null` (it has no NaN or
        // Infinity), so the distinction has to be carried alongside.
        //
        // This is reachable without anyone hand-editing anything: parse_csv
        // takes the cell through `f32::from_str`, which accepts "inf" and
        // turns "1e40" into infinity, and only NaN is the pack's absent
        // sentinel — so `Some(inf)` rides all the way through write_binary.
        // Without this flag the page would report that height as UNKNOWN and
        // draw the node hollow while `planner gaps`, reading the same pack,
        // used infinity as a real antenna height. The two must not disagree
        // about the same node.
        "h_bad": is_present_nonfinite(n.height_agl_m),
        "tx_bad": is_present_nonfinite(n.tx_power_dbm),
        "last_seen": n.last_seen_unix,
    })
}

fn finite_or_null(v: Option<f32>) -> Option<f32> {
    v.filter(|f| f.is_finite())
}

fn is_present_nonfinite(v: Option<f32>) -> bool {
    v.is_some_and(|f| !f.is_finite())
}

async fn nodes_json(State(st): State<Arc<AppState>>) -> Response {
    let out: Vec<serde_json::Value> = st
        .nodes
        .iter()
        .map(|n| {
            let (lon, lat) = st.to_lonlat(Xy { x: n.x as f64, y: n.y as f64 });
            node_json(n, lon, lat)
        })
        .collect();
    axum::Json(serde_json::json!({ "nodes": out })).into_response()
}

#[derive(Deserialize)]
struct NetworkQuery {
    #[serde(default = "d_net_budget")]
    budget_db: f32,
    /// Receiver situation id: street | balcony | indoor-traditional |
    /// indoor-efficient. This is the knob that decides whether the census
    /// answers "who can hear it on the pavement" or "who can hear it in a
    /// flat", and the two differ by 14-31 dB at 868 MHz.
    #[serde(default = "d_situation")]
    situation: String,
    /// Antenna height for nodes whose height nobody has recorded.
    #[serde(default = "d_assume_h")]
    assume_h: f64,
    #[serde(default = "d_net_radius")]
    radius_km: f64,
    #[serde(default = "d_k")]
    k: u8,
    #[serde(default = "d_net_az")]
    azimuths: usize,
}
fn d_net_budget() -> f32 {
    145.5
}
fn d_situation() -> String {
    "street".into()
}
fn d_assume_h() -> f64 {
    8.0
}
fn d_net_radius() -> f64 {
    12.0
}
fn d_k() -> u8 {
    1
}
fn d_net_az() -> usize {
    512
}

fn census_status(st: &NetworkCensus) -> serde_json::Value {
    match st {
        NetworkCensus::Idle => serde_json::json!({ "state": "idle" }),
        NetworkCensus::Computing { started } => serde_json::json!({
            "state": "computing",
            "elapsed_s": started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
        }),
        NetworkCensus::Failed(e) => serde_json::json!({ "state": "failed", "error": e }),
        NetworkCensus::Ready(r) => serde_json::json!({
            "state": "ready",
            "sites": r.sites, "skipped": r.skipped,
            "k_target": r.k_target,
            "assumed_height_m": r.assumed_height_m,
            "situation": r.rx_situation,
            "budget_db": r.budget_db,
            "total": r.total_pop, "served": r.served_pop,
            "served_k": r.served_k_pop, "uncovered": r.uncovered_pop,
            "uniform_weights": r.uniform_weights,
            "backbone_components": r.backbone_components,
            "backbone_isolated": r.backbone_isolated,
            "elapsed_ms": r.elapsed_ms,
            "gaps": r.gaps.iter().map(|g| serde_json::json!({
                "lon": g.0, "lat": g.1, "uncovered": g.2, "area_km2": g.3,
                "nearest_site_m": g.4,
            })).collect::<Vec<_>>(),
        }),
    }
}

async fn network_status(State(st): State<Arc<AppState>>) -> Response {
    axum::Json(census_status(&*st.network.lock().await)).into_response()
}

/// Kick off the census if nothing is running, and report the state either way.
async fn network_start(State(st): State<Arc<AppState>>, Query(q): Query<NetworkQuery>) -> Response {
    {
        let mut g = st.network.lock().await;
        if matches!(*g, NetworkCensus::Computing { .. }) {
            return axum::Json(census_status(&g)).into_response();
        }
        if st.nodes.is_empty() {
            *g = NetworkCensus::Failed(
                "this pack has no deployed-network layer; build it with \
                 `planner pack build --nodes <csv>`"
                    .into(),
            );
            return axum::Json(census_status(&g)).into_response();
        }
        *g = NetworkCensus::Computing { started: Some(std::time::Instant::now()) };
    }

    let st2 = st.clone();
    tokio::task::spawn_blocking(move || {
        let t0 = std::time::Instant::now();
        let out = run_census(&st2, &q);
        let mut g = st2.network.blocking_lock();
        *g = match out {
            Ok(mut r) => {
                r.elapsed_ms = t0.elapsed().as_millis();
                NetworkCensus::Ready(Box::new(r))
            }
            Err(e) => NetworkCensus::Failed(e),
        };
    });
    axum::Json(serde_json::json!({ "state": "computing", "elapsed_s": 0 })).into_response()
}

fn run_census(st: &AppState, q: &NetworkQuery) -> Result<NetworkResult, String> {
    use planner_core::entry_loss::ReceiverSituation;

    let situation = ReceiverSituation::from_id(&q.situation)
        .ok_or_else(|| format!("unknown receiver situation '{}'", q.situation))?;

    // Whole-pack windows: the census is a one-shot job, so it reads the pack
    // rather than a view.
    let lo = st.clamp(Xy { x: st.extent.min_x, y: st.extent.min_y });
    let hi = st.clamp(Xy { x: st.extent.max_x, y: st.extent.max_y });
    let terrain = st.layers.terrain.window(lo, hi).map_err(|e| e.to_string())?;
    let clutter = st.layers.clutter.as_ref().and_then(|l| l.window(lo, hi).ok());
    let population = st.layers.population.as_ref().and_then(|l| l.window(lo, hi).ok());

    let mut assumed = 0usize;
    let sites: Vec<planner_coverage::gaps::SiteSpec> = st
        .nodes
        .iter()
        .map(|n| {
            let h = n.height_agl_m.filter(|v| v.is_finite()).map(|v| v as f64).unwrap_or_else(|| {
                assumed += 1;
                q.assume_h
            });
            planner_coverage::gaps::SiteSpec {
                xy: Xy { x: n.x as f64, y: n.y as f64 },
                h_agl_m: h,
                tx_power_dbm: n.tx_power_dbm.filter(|v| v.is_finite()).map(|v| v as f64),
                label: if n.name.is_empty() { n.id.clone() } else { n.name.clone() },
            }
        })
        .collect();

    let mut link = planner_core::model::LinkParams::eu868_defaults();
    link.delta_n = st.manifest.region.delta_n;
    link.n0 = st.manifest.region.n0;
    link.path_center_lat_deg = st.centre_lat;
    // The receiver situation sets BOTH height and entry loss; setting one
    // without the other is how the earlier maps ended up ambiguous.
    link.rx_h_agl_m = situation.rx_h_agl_m();
    link.entry_loss = situation.entry_loss(link.freq_mhz / 1000.0);

    let link_freq_ghz = link.freq_mhz / 1000.0;
    let mut gp = planner_coverage::gaps::GapParams::defaults(link);
    gp.budget_db = q.budget_db;
    gp.rx_h_agl_m = situation.rx_h_agl_m();
    gp.radius_m = q.radius_km.clamp(1.0, MAX_COVERAGE_RADIUS_KM) * 1000.0;
    gp.k_target = q.k.max(1);
    gp.max_azimuths = Some(q.azimuths.clamp(64, 2048));

    // The census now runs the SAME model as the single-site map.
    //
    // It did not, and the difference showed: with 258 sites at k=1 the map
    // came out flat green over the whole city. Part of that is k=1 having only
    // two colours, but the substance is that `gaps` charged no terminal
    // clutter loss at either end, so every node was credited with a clear
    // horizon it does not have. A node below its own roofline pays 14-17 dB
    // and a street-level listener another 25 — the term that made the
    // single-site map reproduce the shadow an operator reported from their own
    // site.
    //
    // Measured PER SITE from the LoD2 footprints at the same 720 bearings the
    // interactive sweep uses, so the two paths agree by construction rather
    // than by comment. Computed up front, in one pass, because the closure
    // `gaps` calls runs inside a rayon fold and must not touch a lock.
    gp.rx_terminal = Some(planner_coverage::RxTerminal {
        freq_ghz: link_freq_ghz,
        h_agl_m: situation.rx_h_agl_m(),
        ws_m: 27.0,
    });
    {
        type Table = std::sync::Arc<dyn Fn(f64) -> f32 + Send + Sync>;
        const N: usize = 720;
        // Keyed on the position in decimetres: sites are distinct points, and
        // an f64 key would not compare equal after a round trip.
        let key = |p: Xy| ((p.x * 10.0).round() as i64, (p.y * 10.0).round() as i64);
        let mut tables: std::collections::HashMap<(i64, i64), Table> =
            std::collections::HashMap::new();
        if let Ok(guard) = st.buildings.read() {
            if let Some(ix) = guard.get() {
                for site in &sites {
                    // The height `gaps` will actually sweep at, not the raw
                    // advert value: measuring a different antenna than the
                    // sweep uses is the kind of disagreement nobody sees.
                    let h = site.h_agl_m.clamp(1.0, 3000.0);
                    let Some(g) = terrain.sample_bilinear(site.xy).map(|v| v as f64) else {
                        continue;
                    };
                    if !g.is_finite() {
                        continue;
                    }
                    let t: Vec<f32> = (0..N)
                        .map(|i| {
                            let ang = std::f64::consts::TAU * i as f64 / N as f64;
                            ix.clutter_along(site.xy.x, site.xy.y, g, g + h, ang, 120.0, 2.0)
                                .and_then(|(r, ws)| {
                                    planner_propag::p2108::height_gain_correction(
                                        link_freq_ghz,
                                        h,
                                        r,
                                        ws,
                                        planner_propag::p2108::TerminalClutter::Obstructed,
                                    )
                                })
                                .unwrap_or(0.0) as f32
                        })
                        .collect();
                    tables.insert(
                        key(site.xy),
                        std::sync::Arc::new(move |ang: f64| {
                            let f = ang.rem_euclid(std::f64::consts::TAU)
                                / std::f64::consts::TAU;
                            t[((f * N as f64) as usize).min(N - 1)]
                        }) as Table,
                    );
                }
            }
        }
        if !tables.is_empty() {
            gp.tx_terminal = Some(std::sync::Arc::new(move |xy: Xy, _h: f64| {
                tables.get(&key(xy)).cloned()
            }));
        }
    }

    // On the SWEEP pool, never the global one. `analyse` fans its sites into
    // rayon, and on the global pool that owned all twelve threads for the
    // 160 s of a census -- every tile, basemap and overlay request queued
    // behind it, which the operator felt as the map stopping while the census
    // ran. `run_sweep` already observes this rule (cores minus two, so tiles
    // always have a core); the census did not.
    let rep = st
        .sweep_pool
        .install(|| {
            planner_coverage::gaps::analyse(
                &terrain,
                clutter.as_ref(),
                population.as_ref(),
                &sites,
                &gp,
            )
        })
        .map_err(|e| e.to_string())?;

    let gaps = rep
        .gaps
        .iter()
        .take(12)
        .map(|g| {
            let (lon, lat) = st.to_lonlat(g.centroid);
            (lon, lat, g.uncovered_population, g.area_km2, g.nearest_site_m)
        })
        .collect();

    Ok(NetworkResult {
        served: Arc::new(rep.served),
        k_target: gp.k_target,
        sites: rep.sites.len(),
        skipped: rep.skipped.len(),
        assumed_height_m: q.assume_h,
        rx_situation: situation.label().to_string(),
        budget_db: q.budget_db,
        total_pop: rep.population.total,
        served_pop: rep.population.served,
        served_k_pop: rep.population.served_k,
        uncovered_pop: rep.population.uncovered,
        uniform_weights: rep.population.uniform_weights,
        gaps,
        backbone_components: rep.backbone.components.len(),
        backbone_isolated: rep.backbone.components.iter().filter(|c| c.len() == 1).count(),
        elapsed_ms: 0,
    })
}

/// The census raster for a view: "PNT2" | u32 w | u32 h | f64 ox | f64 oy |
/// f64 res_x | f64 res_y | u8 k_target | w*h u8 counts (255 = not evaluated).
///
/// Windowed for the same measured reason as `/loss.bin` — see `window_raster`.
/// The census itself is unchanged: it is still computed over the whole pack at
/// pack resolution, and this only decides which of it crosses the wire.
///
/// The wire format did not change when the server's own raster became u8 — it
/// was already counts with 255 for "not evaluated". `window_raster` still
/// hands back f32 because it is one routine for two rasters (see
/// `ViewSource`), and the round trip is exact: `CountGrid::sample_nearest`
/// maps 255 to NaN and the loop below maps NaN back to 255. The intermediate
/// costs one f32 per WINDOW cell, which the 2048x2048 tile cap bounds at
/// 16 MB and which is freed with the response — not the 392 MB the census
/// raster itself used to be.
async fn network_bin(State(st): State<Arc<AppState>>, Query(q): Query<TileQuery>) -> Response {
    let g = st.network.lock().await;
    let NetworkCensus::Ready(r) = &*g else {
        return (StatusCode::NOT_FOUND, "no census computed yet").into_response();
    };
    let s: &planner_coverage::gaps::CountGrid = &r.served;
    let view = ViewRect { min_x: q.minx, min_y: q.miny, max_x: q.maxx, max_y: q.maxy };
    // Same rule as loss_bin: rayon work under block_in_place, never bare on a
    // tokio worker.
    let (w, h, res_x, res_y, origin, data) =
        tokio::task::block_in_place(|| window_raster(s, &view, q.w, q.h, Resample::Nearest));
    let mut out = Vec::with_capacity(45 + data.len());
    out.extend_from_slice(b"PNT2");
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&origin.x.to_le_bytes());
    out.extend_from_slice(&origin.y.to_le_bytes());
    out.extend_from_slice(&res_x.to_le_bytes());
    out.extend_from_slice(&res_y.to_le_bytes());
    out.push(r.k_target);
    for v in &data {
        // 255 is "not evaluated", which is NOT the same as zero repeaters and
        // must not render as a gap. It is also what a cell outside the census
        // window comes back as, so panning past the swept area shows nothing
        // rather than showing a hole.
        out.push(if v.is_finite() { (*v).clamp(0.0, 254.0) as u8 } else { 255 });
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        out,
    )
        .into_response()
}

async fn wasm_js() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/javascript")],
        include_str!("../static/planner_wasm.js"),
    )
        .into_response()
}

async fn wasm_bin() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/wasm")],
        include_bytes!("../static/planner_wasm_bg.wasm").as_slice(),
    )
        .into_response()
}

async fn pack_info(State(st): State<Arc<AppState>>) -> Response {
    let (lon0, lat0) = st.to_lonlat(Xy { x: st.extent.min_x, y: st.extent.min_y });
    let (lon1, lat1) = st.to_lonlat(Xy { x: st.extent.max_x, y: st.extent.max_y });
    let body = serde_json::json!({
        "name": st.manifest.name,
        "dir": st.pack_dir.display().to_string(),
        "crs_epsg": st.manifest.region.crs_epsg,
        "delta_n": st.manifest.region.delta_n,
        "n0": st.manifest.region.n0,
        "extent": {
            "minx": st.extent.min_x, "miny": st.extent.min_y,
            "maxx": st.extent.max_x, "maxy": st.extent.max_y
        },
        "wgs84": { "min_lon": lon0, "min_lat": lat0, "max_lon": lon1, "max_lat": lat1 },
        "centre": { "lat": st.centre_lat, "lon": st.centre_lon },
        // The pack's native cell size. The page uses it to stop asking for a
        // finer tile than the pack has: zoomed in past native resolution, its
        // resolution check could never be satisfied, so every settled pan and
        // every zoom notch refetched the tile, the basemap, the overlays and
        // the footprints -- measured as five requests for a 220 px pan at a
        // 3 km view, where the held tile already covered it at 5 m.
        "res_m": terrain_res_hint(&st),
        "layers": st.manifest.layers.iter().map(|l| format!("{:?}", l.kind)).collect::<Vec<_>>(),
        // What `/link.json` takes beyond the page's own query, so a caller
        // asks for it only of a sidecar that has it: the query refuses a name
        // it does not know.
        "link_options": ["lean"],
        // And what `/loss/start` takes beyond the page's.
        "loss_options": ["whole", "key"],
        // Where many pairs are asked at once, on a sidecar that has the route.
        "link_batch": "/links.json",
        "licenses": st.manifest.licenses.iter()
            .map(|l| serde_json::json!({"source": l.source, "notice": l.notice}))
            .collect::<Vec<_>>(),
    });
    ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let manifest = match planner_pack::build::inspect(&cli.pack) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pack: {e}");
            std::process::exit(1);
        }
    };
    // Resolve every layer path up front so `manifest` can move into state.
    let path_of = |k: LayerKind| -> Option<PathBuf> {
        manifest.layers.iter().find(|l| l.kind == k).map(|l| cli.pack.join(&l.path))
    };
    let terrain_path = path_of(LayerKind::TerrainDtm)
        .or_else(|| path_of(LayerKind::SurfaceDsm))
        .expect("pack has a terrain layer");
    if cli.index_buildings {
        let Some(p) = path_of(LayerKind::Buildings) else {
            println!("buildings: this pack has no buildings layer, so nothing to index");
            return;
        };
        let m = *CogReader::open(&terrain_path).expect("open terrain").meta();
        if let Err(e) = building_index(&p, (m.origin.x, m.origin.y), &IndexProgress::new()) {
            eprintln!("buildings layer {}: {e}", p.display());
            std::process::exit(1);
        }
        return;
    }
    let clutter_path = path_of(LayerKind::ClutterHeight);
    let population_path = path_of(LayerKind::Population);
    let classes_path = path_of(LayerKind::ClutterClass);
    let built_fraction_path = path_of(LayerKind::BuiltFraction);
    let building_top_path = path_of(LayerKind::BuildingTop);
    let roads: Vec<(planner_pack::roads::Way, [f32; 4])> = path_of(LayerKind::Roads)
        .and_then(|p| std::fs::File::open(p).ok())
        .and_then(|f| planner_pack::roads::read_binary(&mut BufReader::new(f)).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|w| {
            let (mut lo_x, mut hi_x) = (f32::MAX, f32::MIN);
            let (mut lo_y, mut hi_y) = (f32::MAX, f32::MIN);
            for (x, y) in &w.points {
                lo_x = lo_x.min(*x);
                hi_x = hi_x.max(*x);
                lo_y = lo_y.min(*y);
                hi_y = hi_y.max(*y);
            }
            (w, [lo_x, lo_y, hi_x, hi_y])
        })
        .collect();
    if !roads.is_empty() {
        let pts: usize = roads.iter().map(|(w, _)| w.points.len()).sum();
        println!("roads: {} ways / {pts} points loaded", roads.len());
    }
    // Report a places layer that fails to PARSE rather than quietly serving
    // an empty gazetteer: "search finds nothing" and "this pack has no search
    // index" look identical to an operator, and the first one is a bug.
    let places = match path_of(LayerKind::Places) {
        None => planner_pack::places::Gazetteer::default(),
        Some(p) => match std::fs::File::open(&p)
            .map_err(|e| e.to_string())
            .and_then(|f| {
                planner_pack::places::read_binary(&mut BufReader::new(f)).map_err(|e| e.to_string())
            }) {
            Ok(g) => {
                println!(
                    "places: {} names / {} postal areas loaded",
                    g.entries.len(),
                    g.areas.len()
                );
                g
            }
            Err(e) => {
                eprintln!(
                    "places: {} declared in the manifest but unreadable ({e}) — \
                     search disabled. Rebuild the pack with a matching planner version.",
                    p.display()
                );
                planner_pack::places::Gazetteer::default()
            }
        },
    };
    // Same rule as the places layer: a declared-but-unreadable layer is a bug,
    // and reporting it as "no deployed nodes" would show an empty region and
    // invite someone to plan a network that already exists.
    let nodes = match path_of(LayerKind::Nodes) {
        None => Vec::new(),
        Some(p) => match std::fs::File::open(&p).map_err(|e| e.to_string()).and_then(|f| {
            planner_pack::nodes::read_binary(&mut BufReader::new(f)).map_err(|e| e.to_string())
        }) {
            Ok(v) => {
                println!("nodes: {} deployed node(s) loaded", v.len());
                v
            }
            Err(e) => {
                eprintln!(
                    "nodes: {} declared in the manifest but unreadable ({e}) — \
                     the deployed network will show as EMPTY. Rebuild the pack.",
                    p.display()
                );
                Vec::new()
            }
        },
    };
    // LoD2 buildings are indexed AFTER the server is listening, on a
    // background thread. See `AppState::buildings` for why: this is the only
    // startup cost that scales with pack size, and it was holding the bind for
    // half a minute on a full-city 5 m pack.
    let buildings_path = path_of(LayerKind::Buildings);
    let buildings = Arc::new(std::sync::RwLock::new(if buildings_path.is_some() {
        BuildingsIndexState::Loading
    } else {
        BuildingsIndexState::Absent
    }));
    let open = |p: PathBuf| CogReader::open(&p).ok();
    let terrain = CogReader::open(&terrain_path).expect("open terrain");
    let m = *terrain.meta();
    let extent = ViewRect {
        min_x: m.origin.x.min(m.origin.x + m.dx * (m.width as f64 - 1.0)),
        max_x: m.origin.x.max(m.origin.x + m.dx * (m.width as f64 - 1.0)),
        min_y: m.origin.y.min(m.origin.y + m.dy * (m.height as f64 - 1.0)),
        max_y: m.origin.y.max(m.origin.y + m.dy * (m.height as f64 - 1.0)),
    };
    let zone = (manifest.region.crs_epsg % 100) as u8;
    let utm = proj4rs::Proj::from_proj_string(&format!(
        "+proj=utm +zone={zone} +ellps=WGS84 +datum=WGS84 +units=m +no_defs"
    ))
    .expect("utm");
    let ll = proj4rs::Proj::from_proj_string("+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs")
        .expect("ll");
    let mut c = (
        (extent.min_x + extent.max_x) / 2.0,
        (extent.min_y + extent.max_y) / 2.0,
        0.0,
    );
    proj4rs::transform::transform(&utm, &ll, &mut c).ok();

    // Index the buildings behind the bind. A plain OS thread rather than
    // `spawn_blocking`: this runs once, is CPU-bound for seconds when the file
    // must be parsed, and has no business occupying a slot in the pool that
    // serves requests.
    let buildings_progress = Arc::new(IndexProgress::new());
    if let Some(p) = buildings_path {
        let slot = Arc::clone(&buildings);
        let progress = Arc::clone(&buildings_progress);
        // The frame every stored building coordinate is relative to. The pack
        // origin rather than an arbitrary point, so the offsets stay inside
        // the raster's own extent and f32 keeps millimetre resolution.
        let origin = (m.origin.x, m.origin.y);
        std::thread::spawn(move || {
            let state = match building_index(&p, origin, &progress) {
                Ok(idx) => BuildingsIndexState::Ready(Arc::new(idx)),
                Err(e) => {
                    eprintln!(
                        "buildings layer {}: {e} — links fall back to the clutter raster",
                        p.display()
                    );
                    BuildingsIndexState::Absent
                }
            };
            // Published in one write, so a reader sees either the whole index
            // or none of it — never a half-filled one that would answer a link
            // with some buildings missing and no way to tell.
            *slot.write().expect("buildings lock") = state;
        });
    }

    let render_slots = cli
        .render_slots
        .unwrap_or_else(|| {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            cores.saturating_sub(2).max(4)
        })
        .max(1);
    let state = Arc::new(AppState {
        centre_lat: c.1.to_degrees(),
        centre_lon: c.0.to_degrees(),
        manifest,
        layers: Layers {
            terrain: Layer::new(terrain),
            clutter: clutter_path.and_then(open).map(Layer::new),
            population: population_path.and_then(open).map(Layer::new),
            classes: classes_path.and_then(open).map(Layer::new),
            built_fraction: built_fraction_path.and_then(open).map(Layer::new),
            building_top: building_top_path.and_then(open).map(Layer::new),
        },
        roads,
        places,
        nodes,
        buildings,
        buildings_progress,
        extent,
        utm,
        ll,
        slots: Semaphore::new(render_slots),
        link_slots: Semaphore::new(cli.link_slots.unwrap_or(render_slots).max(1)),
        sweep_slots: Semaphore::new(cli.sweep_slots.max(1)),
        sweep_pool: {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            // Two threads held back for interactive work. On a 2-core VPS
            // that would leave nothing, so never go below one.
            let n = cli.sweep_threads.unwrap_or_else(|| cores.saturating_sub(2)).max(1);
            eprintln!(
                "sweeps run on {n} of {cores} thread(s); the rest stay free for tiles"
            );
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .thread_name(|i| format!("sweep-{i}"))
                .build()
                .expect("sweep pool")
        },
        pack_dir: cli.pack.clone(),
        network: Mutex::new(NetworkCensus::default()),
        coverage_cache: Mutex::new(None),
        coverage_dir: cli.coverage_dir.clone(),
        coverage_rasters: std::sync::Mutex::new(Vec::new()),
        sweep: Mutex::new(ProgressiveSweep::default()),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/pack", get(pack_info))
        // Server-rendered fallback (still used if WASM fails to load).
        .route("/view.png", get(view_png))
        .route("/coverage.png", get(coverage_png))
        // Data endpoints for the browser-side renderer.
        .route("/tile.bin", get(tile_bin))
        .route("/basemap.bin", get(basemap_bin))
        .route("/roads.bin", get(roads_bin))
        .route("/buildings.bin", get(buildings_bin))
        .route("/buildings/values.bin", get(buildings_values_bin))
        .route("/buildings/status", get(buildings_status))
        .route("/loss.bin", get(loss_bin))
        .route("/loss/start", get(loss_start))
        .route("/loss/status", get(loss_status))
        .route("/search", get(search))
        .route("/area.bin", get(area_bin))
        .route("/link.json", get(link_json))
        .route(
            "/links.json",
            axum::routing::post(links_json).layer(axum::extract::DefaultBodyLimit::max(32 << 20)),
        )
        .route("/coverage/bands.bin", axum::routing::post(coverage_bands))
        .route("/height.json", get(height_json))
        .route("/api/presets", get(presets_json))
        .route("/nodes.json", get(nodes_json))
        .route("/network/start", get(network_start))
        .route("/network/status", get(network_status))
        .route("/network.bin", get(network_bin))
        .route("/planner_wasm.js", get(wasm_js))
        .route("/planner_wasm_bg.wasm", get(wasm_bin))
        .with_state(state);

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!("planner-web serving {} on http://{addr}", cli.pack.display());
    println!("(local only; no external requests, no bundled browser)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
}

#[cfg(test)]
mod tests {
    /// A sweep waits out a loading index and goes on once it lands; a
    /// superseded one stops waiting, as a cancelled sweep, not a failure.
    #[test]
    fn a_sweep_waits_for_the_building_index() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, RwLock};
        let index = Arc::new(RwLock::new(super::BuildingsIndexState::Loading));
        let landed = Arc::new(AtomicBool::new(false));
        let (i2, l2) = (Arc::clone(&index), Arc::clone(&landed));
        let lander = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            l2.store(true, std::sync::atomic::Ordering::SeqCst);
            *i2.write().unwrap() = super::BuildingsIndexState::Absent;
        });
        super::wait_for_index(&index, &AtomicBool::new(false)).unwrap();
        assert!(landed.load(std::sync::atomic::Ordering::SeqCst), "returned before the index landed");
        lander.join().unwrap();

        let loading = RwLock::new(super::BuildingsIndexState::Loading);
        let err = super::wait_for_index(&loading, &AtomicBool::new(true)).unwrap_err();
        assert!(err.contains("superseded"), "{err}");
    }


    /// The defect: `/link.json` had no way to state either antenna. The budget
    /// carried the shipped `GENERIC_SX1262` placeholder gain applied to BOTH
    /// ends, so an operator with a 5.8 dBi collinear planned with 3.8 dB less
    /// budget than they actually had and nothing in the reply said so.
    #[test]
    fn the_link_budget_follows_the_antennas_the_caller_names() {
        let dflt = resolve_budget(None, None, None);
        assert_eq!(dflt.budget_db, d_budget(), "an untouched URL must not change");
        assert!(!dflt.gains_given);

        // One better antenna buys exactly its excess over the placeholder.
        let one = resolve_budget(None, Some(5.8), None);
        let both_stock = planner_core::preset::max_path_loss_asym(22.0, 2.0, 2.0,
            &planner_core::preset::MC_EU868_DEFAULT, LINK_FADE_MARGIN_DB).unwrap();
        assert!(
            ((one.budget_db as f64) - both_stock - 3.8).abs() < 1e-3,
            "5.8 dBi at one end must buy 3.8 dB, got {:.3}",
            one.budget_db as f64 - both_stock
        );
        assert!(one.gains_given, "the reply must stop calling the gains a placeholder");
        assert_eq!(one.rx_gain_dbi, 2.0, "the unnamed end keeps the preset gain");

        // Both ends named, and the two are added independently.
        let two = resolve_budget(None, Some(5.8), Some(9.0));
        assert!(((two.budget_db as f64) - both_stock - 10.8).abs() < 1e-3);

        // An explicit budget still wins over anything derived.
        let forced = resolve_budget(Some(150.0), Some(5.8), Some(9.0));
        assert_eq!(forced.budget_db, 150.0);
        assert!(forced.source.contains("caller"));
    }

    /// `loc_pct` is the one way to ask for another location percentage, and
    /// a URL without it must keep the number it always had.
    #[test]
    fn the_location_percentage_is_the_models_unless_the_caller_names_one() {
        let dflt = planner_core::model::LinkParams::eu868_defaults().loc_pct;
        assert_eq!(dflt, 90.0);
        assert_eq!(resolve_loc_pct(None), Ok(dflt), "an untouched URL must not change");
        for good in [1.0, 50.0, 99.0] {
            assert_eq!(resolve_loc_pct(Some(good)), Ok(good));
        }
        // Refused here, not handed to the model, whose refusal is a server
        // error, and inside the 0.25 km floor none at all.
        for bad in [0.0, 0.5, 99.5, 100.0, -50.0, f64::NAN, f64::INFINITY] {
            assert!(resolve_loc_pct(Some(bad)).is_err(), "loc_pct {bad} must be refused");
        }
        // As axum parses the query: absent is None, the name is known, and
        // a misspelling is still a 4xx rather than the default.
        let parse = |extra: &str| {
            let uri: axum::http::Uri =
                format!("/link.json?ax=1&ay=2&bx=3&by=4{extra}").parse().unwrap();
            Query::<LinkQuery>::try_from_uri(&uri).map(|Query(q)| q.loc_pct)
        };
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("&loc_pct=50").unwrap(), Some(50.0));
        assert!(parse("&loc=50").is_err());
    }

    /// The near-field model answers inside P.1812's 0.25 km floor and only
    /// there. Every refusal used to be answered from it, as a free-space
    /// figure labelled as a path inside the floor.
    #[test]
    fn only_the_distance_floor_is_answered_from_the_near_field() {
        use planner_core::model::ModelError;
        let near = || Some(71.5);
        assert_eq!(link_model(Ok(120.0), 1.2, near), Ok((120.0, "ITU-R P.1812-8")));
        let floor = || {
            Err(ModelError::OutOfRange("path length 0.150 km outside P.1812's 0.25–3000 km".into()))
        };
        let (lb, model) = link_model(floor(), 0.15, near).unwrap();
        assert!(lb == 71.5 && model.contains("inside P.1812's 0.25 km floor"), "{model}");
        // A path the near-field model cannot take either is still the caller's.
        assert_eq!(link_model(floor(), 0.15, || None).unwrap_err().0, StatusCode::BAD_REQUEST);
        // Past the floor, whatever P.1812 refuses is its error, and the near
        // field is never asked.
        for (refusal, says) in [
            (ModelError::BadProfile("profile spacing 5.00 m is below the 30 m floor".into()),
             "spacing"),
            (ModelError::OutOfRange("frequency 10000 MHz outside P.1812's 30 MHz–6 GHz".into()),
             "frequency"),
        ] {
            let (status, text) =
                link_model(Err(refusal), 1.2, || panic!("the near field was asked")).unwrap_err();
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert!(text.contains(says), "{text}");
        }
    }

    use super::*;
    use planner_pack::nodes::{DeployedNode, NodeKind};

    fn node() -> DeployedNode {
        DeployedNode::new("k1", "B Zionskirche", NodeKind::Repeater, 392_842.0, 5_820_178.0)
    }

    #[test]
    fn an_absent_height_serialises_as_json_null_and_never_as_zero() {
        // The failure this guards is the one that has bitten this repo before:
        // an absent optional collapsing to 0.0, which is indistinguishable
        // from a node measured as sitting on the ground.
        let v = node_json(&node(), 13.4, 52.5);
        assert_eq!(v["h"], serde_json::Value::Null);
        assert_eq!(v["tx_dbm"], serde_json::Value::Null);
        assert_eq!(v["h_bad"], serde_json::json!(false));
    }

    #[test]
    fn a_zero_height_and_a_zero_power_survive_as_real_values() {
        // 0 m AGL is a node on the ground and 0 dBm is 1 mW. Both are legal,
        // so nothing downstream may use a truthiness test on these fields.
        let n = DeployedNode {
            height_agl_m: Some(0.0),
            tx_power_dbm: Some(0.0),
            ..node()
        };
        let v = node_json(&n, 13.4, 52.5);
        assert_eq!(v["h"], serde_json::json!(0.0));
        assert_eq!(v["tx_dbm"], serde_json::json!(0.0));
        assert_ne!(v["h"], serde_json::Value::Null);
    }

    #[test]
    fn a_present_but_infinite_height_is_not_reported_to_the_page_as_unknown() {
        // "1e40" in a community CSV parses to f32::INFINITY, and only NaN is
        // the pack's absent sentinel, so the value reaches this handler
        // present. serde_json writes any non-finite float as `null`, which
        // would make the page say UNKNOWN about a node that `planner gaps`
        // plans with an infinite antenna height.
        let n = DeployedNode {
            height_agl_m: Some(f32::INFINITY),
            tx_power_dbm: Some(f32::NEG_INFINITY),
            ..node()
        };
        let v = node_json(&n, 13.4, 52.5);
        assert_eq!(v["h"], serde_json::Value::Null);
        assert_eq!(v["h_bad"], serde_json::json!(true));
        assert_eq!(v["tx_bad"], serde_json::json!(true));
    }

    #[test]
    fn every_kind_string_the_page_exports_is_read_back_by_from_label() {
        // The page writes this string verbatim into nodes.csv, and parse_csv
        // ERRORS on a kind it does not know — so a prettier spelling here
        // fails the operator's re-import hours later, not at the click.
        for k in [
            NodeKind::Unknown,
            NodeKind::Companion,
            NodeKind::Repeater,
            NodeKind::RoomServer,
            NodeKind::Sensor,
        ] {
            let n = DeployedNode { kind: k, ..node() };
            let s = node_json(&n, 13.4, 52.5)["kind"].as_str().unwrap().to_string();
            assert_eq!(NodeKind::from_label(&s), Some(k), "kind string {s:?}");
        }
    }

    #[test]
    fn the_page_still_reads_the_fields_this_handler_emits() {
        // main.rs and index.html are two halves of one contract with no
        // compiler between them: renaming a field here does not break the
        // build, it silently returns the page to reporting a corrupt height as
        // UNKNOWN, which is the bug h_bad exists to close.
        //
        // The names are read back OUT of node_json rather than written down
        // here, so renaming the field in the handler moves the expectation
        // with it and the test fails only when the page has genuinely not
        // followed. Every emitted key is checked except `id` and `name`, which
        // are too generic to search for as bare substrings.
        let page = include_str!("index.html");
        let v = node_json(&node(), 13.4, 52.5);
        let emitted: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        assert!(emitted.len() >= 10, "node_json emitted only {emitted:?}");
        for field in emitted.iter().filter(|k| !matches!(k.as_str(), "id" | "name")) {
            assert!(
                page.contains(field.as_str()),
                "/nodes.json emits {field:?} but index.html never reads it"
            );
        }
    }

    #[test]
    fn the_exported_lat_lon_is_a_real_inverse_projection_in_lon_lat_order() {
        // The page writes these two straight into the CSV's lat/lon columns.
        // parse_csv rejects |lat| > 90, so a swapped pair fails loudly for
        // Berlin — but not everywhere, and the swap would still put the sweep
        // centre in the wrong hemisphere. Assert the order explicitly.
        let utm = proj4rs::Proj::from_proj_string(
            "+proj=utm +zone=33 +ellps=WGS84 +datum=WGS84 +units=m +no_defs",
        )
        .unwrap();
        let ll =
            proj4rs::Proj::from_proj_string("+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs")
                .unwrap();
        // Zionskirche, Berlin, in the Berlin pack's own EPSG:32633 metres.
        let (lon, lat) = to_lonlat_with(&utm, &ll, Xy { x: 392_842.0, y: 5_820_178.0 });
        assert!((lat - 52.5).abs() < 0.1, "lat was {lat}");
        assert!((lon - 13.4).abs() < 0.1, "lon was {lon}");
    }

    /// A link served while the buildings index is still loading must not look
    /// like one served after it lands.
    ///
    /// The two answers differ — 120.0 dB against 120.7 dB on a measured 1 km
    /// city path, and by more where a Vorderhaus sits on the path — because
    /// the first falls back to the clutter raster. `buildings_indexed: 0` does
    /// not distinguish them: a pack with no Buildings layer reports 0 forever.
    /// So the state has to be nameable, and the three names must stay distinct.
    #[test]
    fn a_loading_index_is_reported_as_neither_ready_nor_absent() {
        assert_eq!(BuildingsIndexState::Loading.label(), "loading");
        assert_eq!(BuildingsIndexState::Absent.label(), "absent");
        assert_eq!(BuildingsIndexState::Ready(Arc::default()).label(), "ready");
        // Only Ready hands out an index; the other two must fall back rather
        // than answer from a half-filled one.
        assert!(BuildingsIndexState::Loading.get().is_none());
        assert!(BuildingsIndexState::Absent.get().is_none());
        assert!(BuildingsIndexState::Ready(Arc::default()).get().is_some());
    }

    /// The background thread parses through `BuildingRecord`, not `Value`.
    ///
    /// `ground_z` is the field that decides absolute roof height, and it is
    /// the one that is optional in the file. If a rename or a `deny_unknown`
    /// ever made this parse fail, every record would count as unparseable and
    /// the index would silently come up empty — the server would still start,
    /// still answer, and quietly be a clutter-raster-only planner.
    #[test]
    fn a_buildings_line_parses_with_and_without_ground_z() {
        let r: BuildingRecord =
            serde_json::from_str(r#"{"e":390015.3,"n":5824938.5,"area_m2":314.16,"height_m":22.0,"ground_z":34.5}"#)
                .expect("full record");
        assert!((r.ground_z + r.height_m - 56.5).abs() < 1e-9);
        // radius from footprint area, the same conversion the thread does
        assert!(((r.area_m2 / std::f64::consts::PI).sqrt() - 10.0).abs() < 0.01);

        let r: BuildingRecord =
            serde_json::from_str(r#"{"e":1.0,"n":2.0,"area_m2":10.0,"height_m":8.0}"#)
                .expect("ground_z is optional");
        assert_eq!(r.ground_z, 0.0);
    }

    /// Lines of a building file that the parse treats each its own way: a
    /// block with a courtyard, a record with no geometry, a footprint too wide
    /// to trust, polygons and rings too short to keep, and two lines that are
    /// not buildings.
    fn building_lines() -> String {
        [
            r#"{"e":1100.0,"n":2100.0,"area_m2":300.0,"height_m":22.0,"ground_z":34.5,"rings":[{"exterior":[[1090.0,2090.0],[1110.0,2090.0],[1110.0,2110.0],[1090.0,2110.0]],"interiors":[[[1095.0,2095.0],[1105.0,2095.0],[1105.0,2105.0]]]}]}"#,
            r#"{"e":1300.5,"n":2050.25,"area_m2":80.0,"height_m":9.5}"#,
            r#"{"e":1500.0,"n":2500.0,"area_m2":50.0,"height_m":12.0,"ground_z":30.0,"rings":[{"exterior":[[1500.0,2500.0],[3200.0,2500.0],[1500.0,2501.0]]}]}"#,
            r#"{"e":1700.0,"n":2700.0,"area_m2":40.0,"height_m":6.0,"rings":[{"exterior":[[1700.0,2700.0],[1701.0,2700.0]]},{"exterior":[[1695.0,2695.0],[1705.0,2695.0],[1705.0,2705.0],[1695.0,2705.0]],"interiors":[[[1700.0,2700.0],[1701.0,2701.0]]]}]}"#,
            "not a record",
            r#"{"e":1.0,"n":2.0,"area_m2":0.0,"height_m":8.0}"#,
        ]
        .join("\n")
            + "\n"
    }

    /// A start that loads the saved index has the index a start that parsed
    /// the file built, value for value: the records bit for bit, the cells a
    /// query reads, and so every footprint and every link.
    #[test]
    fn a_saved_index_is_the_parsed_one_value_for_value() {
        let dir = std::env::temp_dir().join(format!("planner_web_saved_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl = dir.join("buildings.jsonl");
        std::fs::write(&jsonl, building_lines()).unwrap();
        let origin = (1000.0, 2000.0);

        let first = IndexProgress::new();
        let parsed = building_index(&jsonl, origin, &first).unwrap();
        assert!(!first.from_saved.load(Ordering::Relaxed), "nothing was saved to load");
        let saved = saved_index_path(&jsonl);
        assert_eq!(saved, dir.join(".buildings.idx"), "a dot file beside the building file");
        assert!(saved.exists(), "the first start saves the index");
        let second = IndexProgress::new();
        let loaded = building_index(&jsonl, origin, &second).unwrap();
        assert!(second.from_saved.load(Ordering::Relaxed), "the next start loads it");
        assert_eq!(second.json("ready")["source"], "saved index");

        assert_eq!((parsed.count, parsed.with_geometry), (4, 3), "two lines are no buildings");
        let items = |ix: &BuildingIndex| -> Vec<[u32; 7]> {
            ix.items
                .iter()
                .map(|b| {
                    [b.x, b.y, b.r, b.br, b.top_masl]
                        .map(f32::to_bits)
                        .iter()
                        .copied()
                        .chain([b.poly_start, b.poly_end])
                        .collect::<Vec<_>>()
                        .try_into()
                        .unwrap()
                })
                .collect()
        };
        let flat = |v: &[[f32; 2]]| v.iter().flatten().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(items(&loaded), items(&parsed));
        assert_eq!(loaded.cells, parsed.cells);
        assert_eq!((loaded.count, loaded.with_geometry), (parsed.count, parsed.with_geometry));
        assert_eq!(loaded.origin, parsed.origin);
        assert_eq!((&loaded.polys, &loaded.rings), (&parsed.polys, &parsed.rings));
        assert_eq!(flat(&loaded.verts), flat(&parsed.verts));
        assert_eq!(flat(&loaded.area_height), flat(&parsed.area_height));

        // A rewritten file is parsed again, and its index saved over the old.
        std::fs::write(&jsonl, building_lines() + &building_lines()).unwrap();
        let third = IndexProgress::new();
        let again = building_index(&jsonl, origin, &third).unwrap();
        assert!(!third.from_saved.load(Ordering::Relaxed), "the saved index is stale");
        assert_eq!(again.count, 8);
        let fourth = IndexProgress::new();
        assert_eq!(building_index(&jsonl, origin, &fourth).unwrap().count, 8);
        assert!(fourth.from_saved.load(Ordering::Relaxed));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A saved index is used only for the file, grid and version it was
    /// saved for, and only whole: anything else is parsed again rather than
    /// loaded and trusted.
    #[test]
    fn a_saved_index_is_refused_for_any_other_file_grid_or_version() {
        let dir = std::env::temp_dir().join(format!("planner_web_refused_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl = dir.join("buildings.jsonl");
        std::fs::write(&jsonl, building_lines()).unwrap();
        let origin = (1000.0, 2000.0);
        let (ix, _, _) =
            parse_buildings(File::open(&jsonl).unwrap(), origin, &IndexProgress::new());
        std::fs::remove_dir_all(&dir).ok();
        let stamp = FileStamp { bytes: 1234, mtime_s: 1_790_000_000, mtime_ns: 5 };
        let bytes = ix.to_bytes(stamp);
        let load = |b: &[u8], s: FileStamp, o: (f64, f64)| BuildingIndex::from_bytes(b, s, o);
        let refused = |b: &[u8], s: FileStamp, o: (f64, f64)| {
            load(b, s, o).err().expect("a saved index that does not fit is refused")
        };
        let (back, suspect) = load(&bytes, stamp, origin).unwrap();
        assert_eq!((back.count, suspect), (4, 1), "the wide footprint is still suspect");

        for other in [
            FileStamp { bytes: 1235, ..stamp },
            FileStamp { mtime_s: 1_790_000_001, ..stamp },
            FileStamp { mtime_ns: 6, ..stamp },
        ] {
            let why = refused(&bytes, other, origin);
            assert!(why.contains("changed"), "{why}");
        }
        assert!(refused(&bytes, stamp, (1000.0, 2000.000001)).contains("grid"));
        let mut newer = bytes.clone();
        newer[4] = INDEX_VERSION as u8 + 1;
        assert!(refused(&newer, stamp, origin).contains("version"));
        refused(&bytes[..bytes.len() - 1], stamp, origin);
        refused(&[bytes.as_slice(), &[0]].concat(), stamp, origin);
        refused(&bytes[..INDEX_HEADER - 1], stamp, origin);
        // A ring ending past the vertices would panic a footprint query later.
        let last_ring = bytes.len() - ix.verts.len() * 8 - 4;
        let mut corrupt = bytes.clone();
        corrupt[last_ring..last_ring + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(refused(&corrupt, stamp, origin).contains("outside"));
    }

    /// A building's value is read at its centroid from the FULL raster, and a
    /// centroid off the raster reads as "not evaluated", never as zero.
    ///
    /// Zero is a real answer for the census (reached by nobody) and a real
    /// loss (0 dB) is nonsense; conflating "we did not look" with either would
    /// colour a building the map never evaluated.
    #[test]
    fn a_building_off_the_raster_is_not_evaluated_rather_than_zero() {
        let g = ramp(Xy { x: 100.0, y: 200.0 }, 5.0, 10, 10);
        // Inside: the ramp's value is its own x, nearest cell.
        assert_eq!(grid_value_at(&g, 112.0, 190.0), 110.0);
        // Outside, on every side.
        assert!(grid_value_at(&g, 90.0, 190.0).is_nan());
        assert!(grid_value_at(&g, 160.0, 190.0).is_nan());
        assert!(grid_value_at(&g, 112.0, 210.0).is_nan());
        assert!(grid_value_at(&g, 112.0, 140.0).is_nan());
        // The census raster is u8 counts on the same geometry: cell (2, 2)
        // holds 3, and 255 is the "not evaluated" code, not a count of 255.
        let mut counts = counts_grid(Xy { x: 100.0, y: 200.0 }, 5.0, 10, 10);
        counts.data[2 * 10 + 2] = 3;
        counts.data[2 * 10 + 3] = planner_coverage::gaps::NOT_EVALUATED;
        assert_eq!(counts.count_at_world(110.0, 190.0), 3.0);
        assert!(counts.count_at_world(115.0, 190.0).is_nan(), "255 is not a count of 255");
        assert!(counts.count_at_world(90.0, 190.0).is_nan(), "off the raster");
        assert!(counts.count_at_world(160.0, 190.0).is_nan());
        assert!(counts.count_at_world(112.0, 210.0).is_nan());
        assert!(counts.count_at_world(112.0, 140.0).is_nan());
        // No raster at all: NaN for that answer, and independently for the other.
        let (l, c) = building_values(None, Some(&counts), 110.0, 190.0);
        assert!(l.is_nan());
        assert_eq!(c, 3.0);
        let (l, c) = building_values(Some(&g), None, 112.0, 190.0);
        assert_eq!(l, 110.0);
        assert!(c.is_nan());
    }

    /// The values-only route must see exactly the buildings the outline route
    /// sees, or a value refresh would leave some footprints stale forever.
    #[test]
    fn ids_in_agrees_with_outlines_in_about_which_buildings_are_in_a_box() {
        let mut ix = BuildingIndex::default();
        ix.origin = (1000.0, 2000.0);
        // Three square footprints, 10 m across, 100 m apart along x.
        for k in 0..3u32 {
            let cx = 50.0 + 100.0 * k as f32;
            let base = ix.verts.len() as u32;
            for (dx, dy) in [(-5.0, -5.0), (5.0, -5.0), (5.0, 5.0), (-5.0, 5.0)] {
                ix.verts.push([dx, dy]);
            }
            ix.rings.push(base + 4);
            ix.polys.push(ix.rings.len() as u32);
            let b = Bldg {
                x: cx,
                y: 50.0,
                r: 5.0,
                br: 7.1,
                top_masl: 40.0,
                poly_start: ix.polys.len() as u32 - 1,
                poly_end: ix.polys.len() as u32,
            };
            // `insert` returns whether the footprint was SUSPECT (span beyond
            // plausibility), not whether it was accepted; these are 10 m squares.
            assert!(!ix.insert(b));
        }
        // A box that meets the first two buildings only.
        let lo = Xy { x: 1000.0, y: 2000.0 };
        let hi = Xy { x: 1000.0 + 160.0, y: 2000.0 + 100.0 };
        let mut ids = ix.ids_in(lo, hi);
        ids.sort_unstable();
        let (_, lens, _, ring_ids, truncated) = ix.outlines_in(lo, hi, 1_000_000);
        let mut from_outlines: Vec<u32> = ring_ids.clone();
        from_outlines.sort_unstable();
        from_outlines.dedup();
        assert!(!truncated);
        assert_eq!(lens.len(), 2, "two footprints, one ring each");
        assert_eq!(ids, vec![0, 1]);
        assert_eq!(ids, from_outlines);
    }

    /// A reply without values is the reply with values, geometry for
    /// geometry, with NaN ("not evaluated") where the values were: readers
    /// that skip them, as sim-mesh's page does, read the same bytes.
    #[test]
    fn a_footprint_reply_without_values_is_the_same_geometry_with_nan_values() {
        let mut ix = BuildingIndex { origin: (1000.0, 2000.0), ..Default::default() };
        for k in 0..3u32 {
            let base = ix.verts.len() as u32;
            for (dx, dy) in [(-5.0, -5.0), (5.0, -5.0), (5.0, 5.0), (-5.0, 5.0)] {
                ix.verts.push([dx, dy]);
            }
            ix.rings.push(base + 4);
            ix.polys.push(ix.rings.len() as u32);
            let (start, end) = (ix.polys.len() as u32 - 1, ix.polys.len() as u32);
            let x = 50.0 + 100.0 * k as f32;
            let b = Bldg {
                x,
                y: 50.0,
                r: 5.0,
                br: 7.1,
                top_masl: 40.0,
                poly_start: start,
                poly_end: end,
            };
            assert!(!ix.insert(b));
        }
        let (lo, hi) = (Xy { x: 1000.0, y: 2000.0 }, Xy { x: 1300.0, y: 2100.0 });
        let loss = ramp(Xy { x: 1000.0, y: 2100.0 }, 5.0, 60, 21);
        let with = footprints_reply(&ix, lo, hi, Some(&loss), None);
        let without = footprints_reply(&ix, lo, hi, None, None);
        assert_eq!(with.len(), without.len());
        // Header, then per ring: id, count, top, loss, census, 4 vertices.
        let ring = 20 + 4 * 8;
        assert_eq!(with.len(), 25 + 3 * ring);
        for k in 0..3 {
            let at = 25 + k * ring;
            assert_eq!(with[at..at + 12], without[at..at + 12], "id, vertex count and top");
            assert_eq!(with[at + 20..at + ring], without[at + 20..at + ring], "the outline");
            let f = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            assert_eq!(f(&with, at + 12), 1050.0 + 100.0 * k as f32, "the loss at the centroid");
            assert!(f(&with, at + 16).is_nan(), "no census was run");
            assert!(f(&without, at + 12).is_nan() && f(&without, at + 16).is_nan());
        }
        assert_eq!(with[..25], without[..25]);
    }

    /// The validator changes with anything a reply is made from, and only
    /// with that: the file the index was built from, the grid, the box.
    #[test]
    fn a_footprint_validator_follows_the_file_the_grid_and_the_box() {
        let stamp = FileStamp { bytes: 1234, mtime_s: 1_790_000_000, mtime_ns: 5 };
        let ix =
            BuildingIndex { origin: (1000.0, 2000.0), stamp: Some(stamp), ..Default::default() };
        let (lo, hi) = (Xy { x: 1000.0, y: 2000.0 }, Xy { x: 2000.0, y: 3000.0 });
        let etag = footprints_etag(&ix, lo, hi).unwrap();
        assert!(etag.starts_with("\"pbo3-") && etag.ends_with('"'), "{etag}");
        assert_eq!(footprints_etag(&ix, lo, hi).unwrap(), etag, "the same reply, the same tag");
        let other_box = footprints_etag(&ix, lo, Xy { x: 2000.0, y: 3000.5 }).unwrap();
        let rewritten = BuildingIndex {
            stamp: Some(FileStamp { mtime_ns: 6, ..stamp }),
            origin: ix.origin,
            ..Default::default()
        };
        let regridded =
            BuildingIndex { stamp: Some(stamp), origin: (1000.0, 2000.5), ..Default::default() };
        for other in [
            other_box,
            footprints_etag(&rewritten, lo, hi).unwrap(),
            footprints_etag(&regridded, lo, hi).unwrap(),
        ] {
            assert_ne!(other, etag);
        }
        assert!(footprints_etag(&BuildingIndex::default(), lo, hi).is_none(), "no stamp, no tag");

        let ask = |v: &str| {
            let mut h = axum::http::HeaderMap::new();
            h.insert(header::IF_NONE_MATCH, axum::http::HeaderValue::from_str(v).unwrap());
            etag_matches(&h, &etag)
        };
        assert!(ask(&etag));
        assert!(ask(&format!("W/{etag}")), "a weak match is a match for a GET");
        assert!(ask(&format!("\"pbo3-0\", {etag}")), "one of a list");
        assert!(ask("*"));
        assert!(!ask("\"pbo3-0\""));
        assert!(!etag_matches(&axum::http::HeaderMap::new(), &etag), "no header, no match");
    }

    /// A pair answered from a batch's shared read samples exactly what its
    /// own window would have given it: every box inside the shared one, at
    /// points along and around it, to the bit.
    #[test]
    fn a_shared_read_gives_each_pair_its_own_window() {
        let g = planner_terrain::Grid::with_axes(
            Xy { x: 382_644.288_685_023_9, y: 5_824_457.805_585_69 },
            10.0,
            -10.0,
            130,
            300,
            (0..130 * 300).map(|i| ((i as f32) * 0.37).cos() * 25.0 + 35.0).collect(),
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!("planner_web_shared_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.tif");
        planner_terrain::cog::write_geotiff_f32(&path, &g).unwrap();
        let layer = Layer::new(CogReader::open(&path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
        let m = layer.meta;
        let at = |c: f64, r: f64| Xy { x: m.origin.x + c * m.dx, y: m.origin.y + r * m.dy };
        // The batch's box, and pairs' boxes inside it, one across the strip
        // boundary at row 256 and one its whole size.
        let union = (at(3.3, 2.6), at(121.7, 290.2));
        let shared = SharedWindow::read(&layer, union).unwrap();
        for (lo, hi) in [union, (at(10.4, 250.1), at(40.6, 270.3)), (at(80.0, 3.0), at(81.0, 4.0))]
        {
            let own = layer.window(lo, hi).unwrap();
            let view = shared.view((lo, hi)).expect("inside the shared read");
            assert_eq!((view.origin, view.width, view.height), (own.origin, own.width, own.height));
            for k in 0..=500 {
                let t = k as f64 / 500.0;
                let p = Xy { x: lo.x + (hi.x - lo.x) * t + 3.1, y: lo.y + (hi.y - lo.y) * t - 2.9 };
                assert_eq!(
                    view.sample_bilinear(p).map(f32::to_bits),
                    own.sample_bilinear(p).map(f32::to_bits),
                    "box {lo:?}-{hi:?} at {p:?}"
                );
            }
        }
        assert!(shared.view((at(0.0, 0.0), at(5.0, 5.0))).is_none(), "outside the shared read");
    }

    /// `/links.json` asks exactly the pairs named, or one node against every
    /// other, and refuses a request that names neither, both, or a node it
    /// does not have.
    #[test]
    fn a_batch_asks_the_pairs_it_names() {
        let req = |pairs: Vec<[u32; 2]>, from: Option<u32>| LinksRequest {
            nodes: vec![[1.0, 2.0, 3.0]; 4],
            pairs,
            from,
            loc_pct: None,
        };
        assert_eq!(req(vec![[0, 3], [2, 1]], None).pairs().unwrap(), vec![[0, 3], [2, 1]]);
        assert_eq!(req(vec![], Some(2)).pairs().unwrap(), vec![[2, 0], [2, 1], [2, 3]]);
        assert!(req(vec![], None).pairs().is_err(), "nothing asked");
        assert!(req(vec![[0, 1]], Some(2)).pairs().is_err(), "both asked");
        assert!(req(vec![[0, 4]], None).pairs().unwrap_err().contains("node 4"));
        assert!(req(vec![], Some(4)).pairs().unwrap_err().contains("node 4"));
        // As axum reads the body: a misspelled field is refused, not ignored.
        assert!(serde_json::from_str::<LinksRequest>(r#"{"nodes":[],"from":0,"loc":50}"#).is_err());
        let r: LinksRequest =
            serde_json::from_str(r#"{"nodes":[[1,2,3],[4,5,6]],"pairs":[[0,1]],"loc_pct":50}"#)
                .unwrap();
        assert_eq!((r.pairs().unwrap(), r.loc_pct), (vec![[0, 1]], Some(50.0)));
    }

    /// `/height.json`'s building evidence: the buildings whose centroid is
    /// within the radius, each with its record's area and height, and none
    /// from outside it — the estimator then stands the node on the roof it
    /// picks, with planner's mast.
    #[test]
    fn a_node_by_a_roof_is_estimated_on_it() {
        use planner_coverage::environment as env;
        let mut ix = BuildingIndex::default();
        ix.origin = (1000.0, 2000.0);
        // A 400 m² block 22 m tall 10 m from the node, a shed of 9 m² 20 m
        // tall 5 m from it, and a 30 m tower 60 m away.
        for (x, y, area, h) in [(60.0f32, 50.0f32, 400.0f32, 22.0f32), (45.0, 50.0, 9.0, 20.0), (110.0, 50.0, 900.0, 30.0)] {
            let r = (area / std::f32::consts::PI).sqrt();
            assert!(!ix.insert(Bldg { x, y, r, br: r, top_masl: 34.0 + h, poly_start: 0, poly_end: 0 }));
            ix.area_height.push([area, h]);
        }
        let at = (1050.0, 2050.0);
        let mut hints = ix.hints_near(at.0, at.1, env::BUILDING_SEARCH_RADIUS_M);
        hints.sort_by(|a, b| a.footprint_m2.total_cmp(&b.footprint_m2));
        assert_eq!(hints.len(), 2, "the tower is beyond the radius");
        assert_eq!((hints[0].footprint_m2, hints[0].height_m), (9.0, 20.0));
        assert_eq!((hints[1].footprint_m2, hints[1].height_m), (400.0, 22.0));
        assert!((hints[1].xy.x - 1060.0).abs() < 1e-3 && (hints[1].xy.y - 2050.0).abs() < 1e-3);
        let ev = env::SiteEvidence { clutter: None, classes: None, buildings: &hints };
        let e = env::estimate_height(Xy { x: at.0, y: at.1 }, None, &ev, &env::EstimateParams::default());
        assert_eq!(e.basis.kind(), env::BasisKind::Lod2Building, "the shed is no roof to mount on");
        assert!((e.h_agl_m - (22.0 + env::ROOF_MAST_TYPICAL_M)).abs() < 1e-6, "{}", e.h_agl_m);
        // A building indexed without its area and height is not evidence.
        ix.area_height.clear();
        assert!(ix.hints_near(at.0, at.1, env::BUILDING_SEARCH_RADIUS_M).is_empty());
    }

    /// A raster whose value is its own x coordinate, so a shifted window is
    /// arithmetic rather than a judgement call.
    fn ramp(origin: Xy, res: f64, w: usize, h: usize) -> planner_terrain::Grid {
        let mut data = vec![0f32; w * h];
        for r in 0..h {
            for c in 0..w {
                data[r * w + c] = (origin.x + c as f64 * res) as f32;
            }
        }
        planner_terrain::Grid {
            origin,
            dx_m: res,
            dy_m: -res,
            width: w,
            height: h,
            data,
        }
    }

    /// A census raster on the same geometry `ramp` uses, every cell zero.
    fn counts_grid(
        origin: Xy,
        res: f64,
        w: usize,
        h: usize,
    ) -> planner_coverage::gaps::CountGrid {
        planner_coverage::gaps::CountGrid {
            origin,
            dx_m: res,
            dy_m: -res,
            width: w,
            height: h,
            data: vec![0u8; w * h],
        }
    }

    /// The window must sit exactly where the browser will sample it.
    ///
    /// `Grid::origin` is the CENTRE of cell (0,0) and `px_to_world` returns
    /// pixel CENTRES, so the emitted origin carries a half-cell offset. Drop
    /// it and every overlay lands half a cell north-west of the terrain it is
    /// explaining — a misregistration that looks like a propagation result
    /// being wrong about which street it is on, which is the one thing this
    /// map must never be ambiguous about.
    #[test]
    fn a_window_lands_on_the_cells_the_client_will_sample() {
        let src = ramp(Xy { x: 1000.0, y: 2000.0 }, 5.0, 400, 400);
        let view = ViewRect { min_x: 1000.0, min_y: 100.0, max_x: 3000.0, max_y: 2100.0 };
        let (w, h, res_x, res_y, origin, data) =
            window_raster(&src, &view, 200, 200, Resample::Bilinear);
        // Every emitted cell must equal what the client's own sampler would
        // have read from the full raster at that same point.
        for row in 0..h as usize {
            for col in 0..w as usize {
                let p = Xy {
                    x: origin.x + col as f64 * res_x,
                    y: origin.y - row as f64 * res_y,
                };
                let want = src.sample_bilinear(p);
                let got = data[row * w as usize + col];
                match want {
                    Some(v) if v.is_finite() => {
                        assert!((got - v).abs() < 1e-3, "at {col},{row}: {got} vs {v}")
                    }
                    _ => assert!(got.is_nan(), "at {col},{row}: expected NaN, got {got}"),
                }
            }
        }
    }

    /// The empty margin around a small sweep must not be transferred.
    ///
    /// A 2 km sweep seen from a 25 km view was measured at 6.24 MB, of which
    /// about 0.2 MB carried a number and the rest was "not evaluated" repeated
    /// three million times. Clipping is not a size heuristic — the reply
    /// states its own origin, so a smaller rectangle needs no new agreement
    /// with the client.
    #[test]
    fn a_window_carries_no_margin_the_source_cannot_fill() {
        // 500 m of raster inside a 5 km view.
        let src = ramp(Xy { x: 5000.0, y: 5000.0 }, 5.0, 100, 100);
        let view = ViewRect { min_x: 2500.0, min_y: 2000.0, max_x: 7500.0, max_y: 7000.0 };
        let (w, h, _, _, _, data) = window_raster(&src, &view, 500, 500, Resample::Nearest);
        assert!(w <= 60 && h <= 60, "clipped window should be ~50 cells, got {w}x{h}");
        assert!(data.iter().any(|v| v.is_finite()), "the overlap must survive the clip");
        // And the clip must not have thrown away any of the overlap: the
        // source spans 500 m at the view's 10 m step, so ~50 cells each way.
        assert!(w >= 49 && h >= 49, "clip lost part of the overlap: {w}x{h}");
    }

    /// Panning off the swept area returns something the samplers can read.
    ///
    /// A zero-sized `Grid` underflows `width - 1` inside `sample_bilinear`, so
    /// "no overlap" has to be a real one-cell answer rather than an empty one.
    #[test]
    fn no_overlap_is_one_nan_cell_and_not_an_empty_grid() {
        let src = ramp(Xy { x: 0.0, y: 0.0 }, 5.0, 50, 50);
        let view = ViewRect { min_x: 90_000.0, min_y: 90_000.0, max_x: 95_000.0, max_y: 95_000.0 };
        let (w, h, _, _, _, data) = window_raster(&src, &view, 256, 256, Resample::Bilinear);
        assert_eq!((w, h), (1, 1));
        assert_eq!(data.len(), 1);
        assert!(data[0].is_nan());
    }

    /// Each axis is capped against ITS OWN cell size.
    ///
    /// Every pack layer today has square cells, so one figure for both axes
    /// produced the right answer everywhere it was used and the bug could only
    /// appear the day a layer did not. `window_raster` already emits an
    /// independent `res_y`, so the asymmetry is representable now, not later.
    #[test]
    fn a_rasters_two_axes_are_capped_against_their_own_cell_sizes() {
        // 40 km across, 40 km down; 200 m cells east-west, 20 m north-south.
        let view = ViewRect { min_x: 0.0, min_y: 0.0, max_x: 40_000.0, max_y: 40_000.0 };
        let (w, h) = tile_dims(&view, 4096, 4096, 200.0, 20.0);
        assert_eq!(w, 200, "40 km of 200 m cells is 200 across");
        assert_eq!(h, 2000, "40 km of 20 m cells is 2000 down, not 200");
        // The old single-figure form would have returned (200, 200) — the
        // vertical detail silently truncated tenfold.
        let (_, wrong) = tile_dims(&view, 4096, 4096, 200.0, 200.0);
        assert_ne!(h, wrong);
    }

    /// Never finer than the source, and never wider than the cap.
    ///
    /// This is what bounds the transfer by SCREEN size rather than by sweep
    /// radius: a 30 km sweep and a 2 km sweep cost the same at the same zoom.
    #[test]
    fn the_window_is_bounded_by_the_screen_not_by_the_rasters_size() {
        let view = ViewRect { min_x: 0.0, min_y: 0.0, max_x: 40_000.0, max_y: 40_000.0 };
        let small = ramp(Xy { x: 0.0, y: 40_000.0 }, 5.0, 8_000, 8_000);
        let (w1, h1, ..) = window_raster(&small, &view, 1997, 1600, Resample::Bilinear);
        let (w2, h2, ..) = window_raster(&small, &view, 40_000, 40_000, Resample::Bilinear);
        assert_eq!((w1, h1), (1997, 1600), "an honest request is served as asked");
        assert_eq!((w2, h2), (2048, 2048), "and an absurd one is capped, not obeyed");
        // Asking finer than the source has is refused too: 5 m over 40 km is
        // 8000 cells, so a 20 000-cell request cannot buy detail that exists.
        let coarse = ramp(Xy { x: 0.0, y: 40_000.0 }, 200.0, 200, 200);
        let (w3, ..) = window_raster(&coarse, &view, 1997, 1600, Resample::Bilinear);
        assert_eq!(w3, 200, "200 m cells over 40 km is 200 cells, not 1997");
    }

    /// The census tile must be the tile the loss route would have produced.
    ///
    /// `/network.bin`'s source is a `CountGrid` (u8) and `/loss.bin`'s is a
    /// `Grid` (f32); they go through the same `window_raster` precisely so the
    /// three half-cell conventions in its clipping cannot drift apart. This
    /// runs both over the same geometry and the same numbers and demands byte
    /// equality of the emitted tile, including the NaN <-> 255 round trip the
    /// handler relies on.
    #[test]
    fn a_census_tile_matches_the_tile_an_f32_raster_would_have_produced() {
        let origin = Xy { x: 3000.0, y: 9000.0 };
        let (w, h, res) = (120usize, 90usize, 20.0);
        let mut counts = counts_grid(origin, res, w, h);
        for (i, v) in counts.data.iter_mut().enumerate() {
            // A mix of real counts, zeros and the not-evaluated code, so both
            // ends of the mapping are exercised inside the window.
            *v = match i % 9 {
                0 => planner_coverage::gaps::NOT_EVALUATED,
                k => (k - 1) as u8,
            };
        }
        let mut equivalent = ramp(origin, res, w, h);
        for (dst, src) in equivalent.data.iter_mut().zip(counts.data.iter()) {
            *dst = if *src == planner_coverage::gaps::NOT_EVALUATED {
                f32::NAN
            } else {
                *src as f32
            };
        }
        // A view that overhangs the raster on two sides, so the clip runs.
        let view = ViewRect { min_x: 2000.0, min_y: 7000.0, max_x: 5000.0, max_y: 9500.0 };
        let (aw, ah, ax, ay, ao, adata) =
            window_raster(&counts, &view, 300, 300, Resample::Nearest);
        let (bw, bh, bx, by, bo, bdata) =
            window_raster(&equivalent, &view, 300, 300, Resample::Nearest);
        assert_eq!((aw, ah), (bw, bh), "same window size");
        assert_eq!((ax, ay), (bx, by), "same cell size");
        assert_eq!((ao.x, ao.y), (bo.x, bo.y), "same origin");
        // What `network_bin` writes on the wire, from each.
        let pack = |d: &[f32]| -> Vec<u8> {
            d.iter().map(|v| if v.is_finite() { v.clamp(0.0, 254.0) as u8 } else { 255 }).collect()
        };
        assert_eq!(pack(&adata), pack(&bdata));
        // And the tile really carries both kinds of answer, or the comparison
        // above would be an agreement about nothing.
        let bytes = pack(&adata);
        assert!(bytes.iter().any(|b| *b == 255), "some cell is not evaluated");
        assert!(bytes.iter().any(|b| *b > 0 && *b < 255), "and some cell is a real count");
    }

    /// The page rounds with `Math.round`, a half up; the sidecar, standing
    /// in for its arithmetic, must too, or a view cell exactly between two
    /// raster cells would read the other one.
    #[test]
    fn a_half_rounds_up_as_the_page_rounds_it() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(-0.4), 0.0);
        assert_eq!(js_round(0.499_999_999_999_999_94), 0.0);
        assert_eq!(js_round(7.5), 8.0);
        assert_eq!(js_hypot(3.0, 4.0), 5.0);
        assert_eq!(js_hypot(0.0, 0.0), 0.0);
        assert!(js_hypot(f64::NAN, 1.0).is_nan());
    }

    /// A cell's terrain is read as the page read its tile.bin: the nearest
    /// cell, its decimetres a tenth held as an f32, nothing off the tile or
    /// where it has none.
    #[test]
    fn a_terrain_tile_is_read_at_its_nearest_cell() {
        let t = TerrainTile { w: 2, h: 2, ox: 100.0, oy: 200.0, rx: 10.0, ry: 10.0,
                              dm: vec![345, i16::MIN, -7, 0] };
        assert_eq!(t.at(104.0, 196.0), Some(f64::from((345.0f64 * 0.1) as f32)));
        assert_eq!(t.at(106.0, 199.0), None, "no terrain there");
        assert_eq!(t.at(101.0, 189.0), Some(f64::from((-7.0f64 * 0.1) as f32)));
        assert_eq!(t.at(80.0, 200.0), None, "off the tile");
    }

    fn a_raster() -> CoverageRaster {
        let (w, h) = (4usize, 3usize);
        let loss = (0..w * h).map(|i| if i == 5 { NEVER_LOSS } else { 10_000 + 100 * i as u16 }).collect();
        CoverageRaster { w, h, ox: 1000.0, oy: 5000.0, rx: 10.0, ry: 10.0, loss, terrain: None }
    }

    fn a_node(antenna: Option<BandsAntenna>) -> BandsNode {
        BandsNode { key: "0123456789abcdef".into(), x: 1015.0, y: 4990.0, height_m: 10.0, budget_db: 140.0, antenna }
    }

    /// A node's margin at a point is its nearest raster cell's: the budget
    /// and the antenna's gain toward that cell's centre, less the cell's
    /// loss; nothing where the raster evaluated nothing, and outside it no
    /// answer at all.
    #[test]
    fn a_nodes_margin_is_its_nearest_cells() {
        let r = a_raster();
        let plain = a_node(None);
        let m = NodeMargins::new(&r, &plain, 2.0);
        // Cell (2, 1): 3 m off its centre still reads it.
        assert_eq!(m.sample(Xy { x: 1023.0, y: 4988.0 }, Resample::Nearest), Some((140.0 - 106.0) as f32));
        assert!(m.sample(Xy { x: 1011.0, y: 4991.0 }, Resample::Nearest).unwrap().is_nan(), "cell 5 is never");
        assert_eq!(m.sample(Xy { x: 1100.0, y: 4990.0 }, Resample::Nearest), None);
        // With an antenna, its gain toward the cell's centre (a receiver 2 m
        // over the ground at 0 m, the tip 10 m up), whatever point in it.
        let whip = BandsAntenna { directional: false, peak_dbi: 2.0, vbw_deg: 75.0, tilt_deg: 10.0, hbw_deg: None,
                                  floor_db: 18.0, azimuth_deg: 0.0, elevation_deg: 0.0 };
        let node = a_node(Some(whip));
        let m = NodeMargins::new(&r, &node, 2.0);
        let (_, el) = tip_direction(1015.0, 4990.0, 10.0, 1020.0, 4990.0, 2.0);
        let gain = node.antenna.as_ref().unwrap().gain(0.0, el);
        let want = (140.0 + gain - 106.0) as f32;
        assert_eq!(m.sample(Xy { x: 1023.0, y: 4988.0 }, Resample::Nearest), Some(want));
        assert_eq!(m.sample(Xy { x: 1017.0, y: 4994.0 }, Resample::Nearest), Some(want));
    }

    /// `window_cells` is `window_raster` on the cells it is given: where the
    /// screen asks no finer than the source, the two are one window.
    #[test]
    fn a_window_of_given_cells_is_the_window_raster_gives() {
        let src = ramp(Xy { x: 1000.0, y: 2000.0 }, 5.0, 400, 400);
        let view = ViewRect { min_x: 900.0, min_y: 0.0, max_x: 2900.0, max_y: 2200.0 };
        let (w, h) = tile_dims(&view, 200, 220, 5.0, 5.0);
        let a = window_raster(&src, &view, 200, 220, Resample::Bilinear);
        let b = window_cells(&src, &view, w, h, Resample::Bilinear);
        assert_eq!((a.0, a.1, a.2, a.3, a.4.x, a.4.y), (b.0, b.1, b.2, b.3, b.4.x, b.4.y));
        assert!(a.5.iter().zip(&b.5).all(|(p, q)| p.to_bits() == q.to_bits()));
        // And it keeps cells finer than the source's when asked for them.
        let (fw, ..) = window_cells(&src, &view, 4000, 220, Resample::Nearest);
        assert!(fw > 2000, "{fw}");
    }

    /// The names the bands route reads files by are the store's, so a
    /// request cannot walk out of the coverage cache.
    #[test]
    fn a_bands_request_names_only_store_names_and_keys() {
        assert!(valid_name("berlin-centre") && valid_name("a") && valid_name("x9"));
        let long = "a".repeat(33);
        for bad in ["", "-a", "a-", "A", "a/b", "..", "a.b", long.as_str()] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_key("0123456789abcdef"));
        for bad in ["0123456789abcde", "0123456789abcdeg", "0123456789ABCDEF", "../../etc/passwd"] {
            assert!(!valid_key(bad), "{bad:?}");
        }
    }
}
