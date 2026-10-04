/* What SlippyMap draws over its tiles: GeoJSON polygons in degrees, each
 * with its fill, outline and label. */

export type Geometry = { type: string; coordinates: unknown } | null

export interface MapShape {
  geometry: Geometry
  fill?: string
  stroke?: string
  width?: number
  dash?: number[]
  /** Written inside its first ring's top-left corner. */
  label?: string
}

/** A rectangle [west, south, east, north] as a polygon. */
export function boxGeometry(b: readonly number[]): Geometry {
  const [w, s, e, n] = b as [number, number, number, number]
  return { type: 'Polygon', coordinates: [[[w, s], [e, s], [e, n], [w, n], [w, s]]] }
}
